//! The sessions collection: `POST /api/sessions` validates the body, writes one `starting` row and
//! answers 202 (HS-008, ADR-0012); `GET /api/sessions` answers every row in the repository's own
//! order together with the machine's network default (HS-013, ADR-0017); `GET /api/sessions/{id}`
//! answers one row, which is what the session screen opens with (HS-014); and
//! `POST /api/sessions/{id}/prompt` starts a turn and answers 202 with the now-`running` row
//! (HS-015); `POST /api/sessions/{id}/abort` accepts an abort and answers 202 with the row while the
//! ladder ends the turn off-request (HS-016); `POST /api/sessions/{id}/steer` hands guidance to the
//! running turn and records it as the event the transcript reads (HS-017); and
//! `POST /api/sessions/{id}/resume` brings a stopped (or failed) session back through the provisioner
//! (HS-018); and `DELETE /api/sessions/{id}` removes a session once its uncommitted work is safe
//! (HS-019).
//!
//! The body is parsed by hand rather than through `axum::Json<T>`: that extractor rejects a bad
//! body with its own `text/plain` answer, which would break ADR-0008's envelope on exactly the
//! 422 path this endpoint exists to get right. The handler also never holds the database's
//! `MutexGuard` across an `.await`; the lock scope is one synchronous block, and the compiler
//! enforces it because a guard is not `Send`.

use std::sync::PoisonError;

use axum::Json;
use axum::body::Bytes;
use axum::extract::RawQuery;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::SessionBackend;
use crate::backup::Backups;
use crate::clock;
use crate::deleter;
use crate::events::NewEvent;
use crate::http::app::AppState;
use crate::http::error::{ApiError, internal};
use crate::ladder;
use crate::sessions::activity;
use crate::sessions::state::{StateError, Trigger};
use crate::sessions::{NewSession, RepoError, Session, Status};
use crate::worktree::Refusal;

/// The header a create's idempotency key travels in.
const KEY_HEADER: &str = "idempotency-key";

/// What a prompt is refused with: the sentence names the request, because an envelope that reads
/// like another endpoint's is how a client builder picks the wrong branch.
const NOT_IDLE_PROMPT: &str = "the session is not idle, so it cannot be prompted";

/// What an abort is refused with. `abort` is the one request that needs a *running* turn, so a
/// session in any other state is refused rather than answered with a 202 that does nothing
/// (spec item 16: abort stops a turn, and there is no turn otherwise).
const NOT_RUNNING_ABORT: &str = "the session is not running, so it has no turn to abort";

/// What a steer is refused with, for the same reason: guidance belongs to a turn that is running.
const NOT_RUNNING_STEER: &str = "the session is not running, so there is no turn to steer";

/// What a resume is refused with. The transition table has exactly two `resume` rows — from `stopped`
/// and from `error` — so those are the two states that accept one (HS-018).
const NOT_RESUMABLE: &str =
    "the session is neither stopped nor in error, so there is nothing to resume";

/// What a delete is refused with when the table has no delete row for the row's state.
///
/// The table's row is `any + delete`, so today this sentence is unreachable; it exists because a
/// refusal must answer *something*, and a code derived from the state is the family's rule
/// (ADR-0019) rather than a panic on a path a future row could open.
const NOT_DELETABLE: &str = "the session is in a state that cannot be deleted";

/// The event a steer is recorded as (HS-017, ADR-0021), under the name it has with every other kind:
/// `<subject>.<verb>`, like `turn.start`, and the subject is the turn, because the operator's guidance
/// is something that happened *to a turn* rather than a message the agent produced. The payload is
/// `{"text": "…"}` — no message id, because a steer is one frame that is never continued or replaced,
/// so the block the renderer draws has nothing to be keyed by. The name lives in [`crate::events`];
/// this is an import so the handler reads a word, not a literal.
use crate::events::TURN_STEER;

/// How many characters a key may hold. Generous for a UUID, small enough that the unique column
/// cannot become a payload.
const MAX_KEY_CHARS: usize = 200;

/// The reason recorded when a create stops an idle session to free its slot (HS-020, spec item 9).
///
/// The row is `stopped` with this word, which is what the Sessions screen turns into "nothing was
/// deleted, resume it to continue": an eviction is the machine making room, never data loss.
pub const EVICTED: &str = "evicted";

/// The body's fields, exactly: any other key is `unknown_field`.
const FIELDS: [&str; 4] = ["project", "model", "thinking", "network"];

/// A prompt's body: one field, exactly, and a body that is anything else is `bad_body` (D1). No
/// length cap beyond axum's own 2 MiB: the spec sets none, and a cap would be an invented number.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptBody {
    prompt: String,
}

/// A steer's body: one `guidance` string and nothing else (HS-017). The word is the composer's own
/// field name and the glossary's **Steer**, so the page and the API say the same thing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SteerBody {
    guidance: String,
}

/// `NAME_MAX`'s 255: HS-025 builds `<projects_root>/<project>` from this value.
const MAX_PROJECT_CHARS: usize = 255;

const MAX_MODEL_CHARS: usize = 128;

const MAX_THINKING_CHARS: usize = 32;

/// The body of a create. `network` defaults to `false` when absent, unconditionally: the
/// config's `network_default` is the UI checkbox's default, not an API body's.
#[derive(Debug, Deserialize)]
struct CreateBody {
    project: String,
    model: String,
    thinking: String,
    #[serde(default)]
    network: bool,
}

/// Read the body as JSON, refuse an unknown field, then deserialise.
///
/// No `deny_unknown_fields`: the key-set check is here so `unknown_field` is a decision and not
/// a string match on serde's error text.
fn parse_body(bytes: &[u8]) -> Result<CreateBody, ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ApiError::bad_body())?;
    let object = value.as_object().ok_or_else(ApiError::bad_body)?;
    if object.keys().any(|field| !FIELDS.contains(&field.as_str())) {
        return Err(ApiError::unknown_field());
    }
    serde_json::from_value(value).map_err(|_| ApiError::bad_body())
}

/// The create's key: exactly one header value, non-empty, at most 200 characters, stored
/// verbatim (the header's bytes as HTTP parsed them, with no folding or normalisation).
fn idempotency_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    let mut values = headers.get_all(KEY_HEADER).iter();
    let value = values
        .next()
        .ok_or_else(ApiError::missing_idempotency_key)?;
    if values.next().is_some() {
        return Err(ApiError::bad_idempotency_key());
    }
    let key = value
        .to_str()
        .map_err(|_| ApiError::bad_idempotency_key())?;
    if key.is_empty() {
        return Err(ApiError::missing_idempotency_key());
    }
    if key.chars().count() > MAX_KEY_CHARS {
        return Err(ApiError::bad_idempotency_key());
    }
    Ok(key)
}

/// `project` is one path segment: non-empty, at most 255 characters, letters, digits, `.`, `_`
/// and `-`, and not `.` or `..`. Whether a repository exists there is HS-025's check.
fn validate_project(project: &str) -> Result<(), ApiError> {
    if project.is_empty()
        || project.chars().count() > MAX_PROJECT_CHARS
        || project == "."
        || project == ".."
        || !project.bytes().all(is_project_byte)
    {
        return Err(ApiError::bad_project());
    }
    Ok(())
}

fn is_project_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

/// `model` is 1–128 characters of letters, digits, `.`, `:`, `/` and `-`. No list of models is
/// invented here: pi's real values are HS-031's.
fn validate_model(model: &str) -> Result<(), ApiError> {
    if model.is_empty()
        || model.chars().count() > MAX_MODEL_CHARS
        || !model.bytes().all(is_model_byte)
    {
        return Err(ApiError::bad_model());
    }
    Ok(())
}

fn is_model_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'/' | b'-')
}

/// `thinking` is 1–32 lowercase letters, digits, `_` and `-`, starting with a letter. No list of
/// levels is invented here either (HS-031).
fn validate_thinking(thinking: &str) -> Result<(), ApiError> {
    let starts_with_letter = thinking
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase());
    if !starts_with_letter
        || thinking.chars().count() > MAX_THINKING_CHARS
        || !thinking.bytes().all(is_thinking_byte)
    {
        return Err(ApiError::bad_thinking());
    }
    Ok(())
}

fn is_thinking_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
}

/// A create's decisions in their fixed order: body shape and unknown field (`parse_body`), the
/// key, then project, model, thinking (ADR-0012). Pinned by `the_precedence_is_...` below,
/// because an order that is not written down is an order that changes when the code moves.
fn request_parts<'a>(
    headers: &'a HeaderMap,
    body: &[u8],
) -> Result<(CreateBody, &'a str), ApiError> {
    let parsed = parse_body(body)?;
    let key = idempotency_key(headers)?;
    validate_project(&parsed.project)?;
    validate_model(&parsed.model)?;
    validate_thinking(&parsed.thinking)?;
    Ok((parsed, key))
}

/// The row on the wire plus the one key the table does not have: where its event stream will be
/// served (HS-010). Flattening `Session` is what keeps the body the row rather than a hand-built
/// copy of it.
#[derive(Debug, Serialize)]
pub struct CreatedSession {
    #[serde(flatten)]
    session: Session,
    events: String,
}

impl CreatedSession {
    fn of(session: Session) -> Self {
        Self {
            events: format!("/api/sessions/{}/events", session.id),
            session,
        }
    }
}

/// Answer a create: 202, `Location`, and the row plus `events`.
///
/// The body arrives as a `Result` because the `Bytes` extractor's rejection is otherwise axum's
/// `text/plain` answer, outside ADR-0008's envelope.
pub async fn create(
    state: &AppState,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        // Every rejection — a body hyper could not buffer, or one over axum's 2 MiB default — is
        // `bad_body`. There is no 413 code and no configured limit, and a refusal that carries a
        // code is more use to a client than a bodiless plain-text 413 (ADR-0008, ADR-0012).
        Err(_) => return ApiError::bad_body().into_response(),
    };
    let (parsed, key) = match request_parts(&headers, &body) {
        Ok(parts) => parts,
        Err(error) => return error.into_response(),
    };
    // The project's refusals are read here, on the request path, and *answered* only where the create
    // would write a row (HS-026): a retry of an idempotency key creates nothing and is answered from the
    // row it already has, and a refused create must not evict an idle session on its way to saying no.
    let refusal = refuse_project(state, &parsed.project).await;
    match slot_for(state, &parsed, key, refusal).await {
        Ok((session, inserted)) => {
            // The handoff, and the only place this story spawns anything. The answer is built
            // first, from the `starting` row this call just wrote, and only then does the
            // provisioner get the session on another task — so the 202 can never wait for a step,
            // and the body can never show a state a step has already moved past. A retry of the
            // same key inserts nothing and is answered from the row it already has, so one
            // session is provisioned once.
            let response = created(session.clone());
            if inserted {
                // Spec item 1's `project_backup`: the day's bundle is taken **before** the session can
                // touch the repository, because the whole point of the recovery point is that it predates
                // the agent. It is awaited rather than spawned so that ordering is a fact, and it is
                // awaited *here* rather than on the request's first line so a retry, a refusal and a
                // full machine do not pay for it.
                back_up(state, &session.project).await;
                state.provisioner().spawn(session);
            }
            response
        }
        Err(error) => error.into_response(),
    }
}

/// Take today's bundle of the project's repository, if this machine keeps one and has not today (HS-065).
///
/// Spec item 1's `project_backup` is a safety net under the shared repository: several sessions work in
/// one checkout's branches, so an agent that damages it damages every session's base. The bundle is taken
/// once per project per day, on the host, before the session that first needs it can run.
///
/// **Every failure is logged and swallowed.** The story says so — "failure to create a bundle is logged
/// and does not block session creation" — and the alternative would be a machine whose disk is full
/// refusing to start work, which turns a safety net into an outage. The repository is
/// `<projects_root>/<project>` whatever backend this run uses: the bundle is of the host checkout, which
/// is where every session's branch lives.
async fn back_up(state: &AppState, project: &str) {
    if !state.config.project_backup {
        return;
    }
    let backups = Backups::new(&state.config.data_root);
    let repository = crate::worktree::repository_of(&state.config.projects_root, project);
    let project = project.to_string();
    let named = project.clone();
    // On a blocking task for the reason the project's refusals are: a bundle is a subprocess, and a
    // create's request path must not park a runtime worker on one.
    let taken =
        tokio::task::spawn_blocking(move || backups.take(&repository, &named, clock::unix_now()))
            .await;
    match taken {
        Ok(Ok(Some(bundle))) => tracing::info!(
            target: "holosmith::http::sessions",
            project = %project,
            bundle = %bundle.display(),
            "took the day's project bundle"
        ),
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::warn!(
            target: "holosmith::http::sessions",
            project = %project,
            %error,
            "the project's backups are not up to date; the session is created anyway"
        ),
        Err(error) => tracing::warn!(
            target: "holosmith::http::sessions",
            project = %project,
            %error,
            "the backup task did not run; the session is created anyway"
        ),
    }
}

/// The project's refusal rules, read before the create writes anything (HS-026).
///
/// The read is two filesystem questions and one `git config`, and it runs on a blocking task: a create's
/// request path must not park a runtime worker on a subprocess, which is the same rule the worktree's
/// git reads follow (ADR-0029).
///
/// `None` means the project may be mounted — or that this run's backend mounts no repository at all,
/// which is the stub's answer (`Backends::refuse`). A `Some` is the envelope the create answers **if** it
/// would otherwise write a row, so a retry of a key that already names a session, and a create that
/// finds the machine full, are unaffected.
async fn refuse_project(state: &AppState, project: &str) -> Option<ApiError> {
    let backend = state.backend.clone();
    let project = project.to_string();
    match tokio::task::spawn_blocking(move || backend.refuse_project(&project)).await {
        Ok(Ok(())) => None,
        Ok(Err(refusal)) => Some(refusal_error(refusal)),
        // A task that panicked or was cancelled has said nothing about the project, and `internal` is
        // what logs the operation and the error once (ADR-0012).
        Err(error) => Some(internal("refuse_project", error)),
    }
}

/// The envelope a [`Refusal`] is answered with.
///
/// A repository git could not read is not something the operator can fix from the phone: git's own words
/// go to the log, and the client gets the one sentence every unplanned failure gets (ADR-0008).
fn refusal_error(refusal: Refusal) -> ApiError {
    match refusal {
        Refusal::NotADirectory => ApiError::git_not_directory(),
        Refusal::SubmodulesPresent => ApiError::submodules_present(),
        Refusal::WorktreeConfigEnabled => ApiError::worktree_config_enabled(),
        // A repository git cannot read has not been checked at all, and git's words are the operator's:
        // `internal` logs the operation and the error, and the client gets ADR-0008's fixed sentence.
        Refusal::Unreadable(error) => internal("refuse_project", error),
    }
}

/// What one attempt at the create's single lock scope found.
///
/// Three answers rather than an `Option` or a sentinel: the machine can have room (insert), the key can
/// already name a session (a retry, answered from the row it has), or it can be full (evict, then try
/// again) — and each has a different next step, which is why the handler cannot guess from a `bool`.
enum Slot {
    /// A free slot was found and the row written: this call owns provisioning it.
    Inserted(Session),
    /// The idempotency key named a session already: nothing was inserted and nothing is provisioned.
    Existing(Session),
    /// The machine runs `max_live_sessions` sessions already: no row was written.
    Full,
}

/// Insert the session, stopping the least recently active idle one while the machine is full (HS-020).
///
/// The loop is the handler's and not the insert scope's: an eviction moves a row through
/// [`Transitioner::apply`](crate::sessions::state::Transitioner::apply), which takes the database lock
/// itself, so it cannot run inside `create_locked`'s one scope without a second way to write a state
/// (ADR-0016). Each successful eviction strictly lowers the live count, so the loop terminates; the
/// bound is there for a create racing another create, where the honest answer is the refusal rather
/// than a wait that has no end.
///
async fn slot_for(
    state: &AppState,
    body: &CreateBody,
    key: &str,
    refusal: Option<ApiError>,
) -> Result<(Session, bool), ApiError> {
    // `saturating_add`, not `+ 1`: the bound is a safety net, and an arithmetic panic in a request
    // path for a number nobody can reach is still a panic waiting for a 32-bit build.
    let bound = usize::try_from(state.config.max_live_sessions)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    for _ in 0..bound {
        match create_locked(state, body, key, refusal.as_ref())? {
            Slot::Inserted(session) => return Ok((session, true)),
            Slot::Existing(session) => return Ok((session, false)),
            Slot::Full => {
                if !evict_one(state).await {
                    return Err(ApiError::cap_without_idle());
                }
            }
        }
    }
    // Every attempt evicted and the machine was still full: only two creates racing each other can
    // reach this, and `cap_without_idle` is the answer a client can act on.
    Err(ApiError::cap_without_idle())
}

/// Stop the least recently active idle session to free a slot, and say whether one was stopped.
///
/// The row moves before the container (ADR-0020) with the reason [`EVICTED`], and the backend's stop
/// is best effort: the slot is free either way, and a container that outlives a `stopped` row is
/// something the operator can still delete. Nothing else about the session is touched — its events,
/// its worktree path and its history are what a stop leaves alone (spec item 9).
///
/// `false` means no slot was freed: nothing was idle, or the candidate started a turn between the read
/// and the move, or the database refused. The caller answers the cap's refusal in every case, which is
/// the sentence that is still true.
async fn evict_one(state: &AppState) -> bool {
    let candidate = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match database.sessions().oldest_idle() {
            Ok(Some(session)) => session,
            Ok(None) => {
                tracing::debug!(
                    target: "holosmith::http",
                    operation = "evict",
                    "the machine is full and no session is idle"
                );
                return false;
            }
            Err(error) => {
                tracing::error!(
                    target: "holosmith::http",
                    operation = "oldest_idle",
                    %error,
                    "the eviction candidate could not be read"
                );
                return false;
            }
        }
    };
    match state
        .transitioner()
        .apply(&candidate.id, Trigger::Evict, Some(EVICTED))
        .await
    {
        Ok(_) => {}
        // The row is `stopped` and only the report of it was lost (a process on its way down), so the
        // container must still be stopped.
        Err(StateError::Unreported(_)) => {}
        Err(error) => {
            // `Illegal` here is the ordinary race — the session was prompted between the read and the
            // move — and a repository failure is the database's. Both answer the same way.
            tracing::info!(
                target: "holosmith::http",
                session = %candidate.id,
                %error,
                "the eviction did not happen"
            );
            return false;
        }
    }
    // The row is `stopped`, which is what frees the slot, so the create must not wait for the
    // container: spec item 7's rule (an answer never waits for container work) applies here exactly
    // as it does to provisioning. The order still holds — the row moved first — and the stop is best
    // effort in a task of its own, like the ladder's second rung, with the same "a container that
    // outlived its row is still deletable" reason.
    let stopping = state.backend.clone();
    let victim = candidate.clone();
    tokio::spawn(async move {
        if let Err(error) = stopping.stop(&victim).await {
            tracing::warn!(
                target: "holosmith::http",
                session = %victim.id,
                %error,
                "the evicted session's backend was not stopped"
            );
        }
    });
    tracing::info!(
        target: "holosmith::http",
        session = %candidate.id,
        "evicted the least recently active idle session"
    );
    true
}
/// The one lock scope: lookup, the project's refusals, cap count and insert, with no `.await` inside so
/// the guard cannot outlive a synchronous statement. `live_count` and `create` share it, so two creates
/// cannot both see a free slot (ADR-0012).
///
/// `Slot::Existing` means the idempotency key already named a session; `Slot::Full` means the caller
/// may evict one and try again. `refusal` is the project's answer, read before the lock (HS-026): it is
/// raised between the key's lookup and the cap, because a create that cannot run must answer its own
/// refusal rather than stop somebody else's session to make room for work that will not happen.
fn create_locked(
    state: &AppState,
    body: &CreateBody,
    key: &str,
    refusal: Option<&ApiError>,
) -> Result<Slot, ApiError> {
    let database = state
        .database
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let repository = database.sessions();
    // The lookup runs before the cap: a retry whose first attempt filled the last slot must get
    // its own session, not 409 — and, since HS-020, must not evict a session on the way to finding
    // out it already had one.
    if let Some(existing) = repository
        .by_client_key(key)
        .map_err(|error| internal("by_client_key", error))?
    {
        return Ok(Slot::Existing(existing));
    }
    if let Some(refusal) = refusal {
        return Err(refusal.clone());
    }
    let live = repository
        .live_count()
        .map_err(|error| internal("live_count", error))?;
    if live >= u64::from(state.config.max_live_sessions) {
        return Ok(Slot::Full);
    }
    let new = NewSession {
        project: body.project.clone(),
        model: body.model.clone(),
        thinking: body.thinking.clone(),
        network: body.network,
        client_key: Some(key.to_string()),
        base_commit: None,
    };
    match repository.create(&new, clock::unix_now()) {
        Ok(session) => Ok(Slot::Inserted(session)),
        Err(RepoError::DuplicateClientKey) => match repository.by_client_key(key) {
            Ok(Some(winner)) => Ok(Slot::Existing(winner)),
            Ok(None) => {
                // The insert said the key exists and the re-read says it does not; a retry must
                // never see an error for a race it cannot fix, so this is not a client error.
                tracing::error!(
                    target: "holosmith::http",
                    operation = "create",
                    "a duplicate client key answered no session"
                );
                Err(ApiError::internal())
            }
            Err(error) => Err(internal("by_client_key", error)),
        },
        Err(error) => Err(internal("create", error)),
    }
}

fn created(session: Session) -> Response {
    let location = format!("/api/sessions/{}", session.id);
    let mut response = (StatusCode::ACCEPTED, Json(CreatedSession::of(session))).into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location).expect("a session id is a valid header value"),
    );
    response
}

/// The list's body: every row in the repository's own order, plus the machine setting the
/// new-session form's Internet-access checkbox defaults from.
///
/// `network_default` rides here rather than on `/api/health` because that payload is pinned at four
/// keys (ADR-0009) and because the screen needs both answers to draw itself: one round trip, no new
/// endpoint (ADR-0017). The elements are the `Session` row itself, so the list cannot drift from the
/// create's answer.
#[derive(Debug, Serialize)]
pub struct SessionList {
    sessions: Vec<Session>,
    network_default: bool,
}

/// Answer `GET /api/sessions`: 200 and the collection, or the ADR-0008 envelope.
///
/// A read-only route, so the guards apply with nothing added here (ADR-0010).
pub async fn list(state: &AppState) -> Response {
    match list_locked(state) {
        Ok(list) => Json(list).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The one lock scope the list has: no `.await` inside it, so the guard cannot outlive a statement.
///
/// No filter and no paging: the repository's state filter has no caller yet and HS-061 owns paging,
/// so the answer is the order `list(None)` already returns (`last_active_at DESC, id ASC`).
fn list_locked(state: &AppState) -> Result<SessionList, ApiError> {
    let database = state
        .database
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let sessions = database
        .sessions()
        .list(None)
        .map_err(|error| internal("list", error))?;
    Ok(SessionList {
        sessions,
        network_default: state.config.network_default,
    })
}

/// Answer `GET /api/sessions/{id}`: 200 and that row, or ADR-0008's envelope.
///
/// The session screen opens with this and nothing else: the list's elements are the row, so the one
/// row is the same body, and the stream the screen then opens is a path the page spells itself
/// (ADR-0018). An unknown id is `not_found`, which is also what the stream answers for one.
///
/// A read-only route, so the guards apply with nothing added here (ADR-0010).
pub async fn one(state: &AppState, id: &str) -> Response {
    match one_locked(state, id) {
        Ok(Some(session)) => Json(session).into_response(),
        Ok(None) => ApiError::unknown_session().into_response(),
        Err(error) => error.into_response(),
    }
}

/// The one lock scope this answer has, for the same reason the list has one: no `.await` inside it.
fn one_locked(state: &AppState, id: &str) -> Result<Option<Session>, ApiError> {
    let database = state
        .database
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    database
        .sessions()
        .get(id)
        .map_err(|error| internal("get", error))
}

/// A prompt's body: the one field, non-empty. Anything else — a non-object, a missing or empty
/// `prompt`, a second field — is `bad_body` (D1).
fn parse_prompt(bytes: &[u8]) -> Result<String, ApiError> {
    let body: PromptBody = serde_json::from_slice(bytes).map_err(|_| ApiError::bad_prompt())?;
    if body.prompt.trim().is_empty() {
        return Err(ApiError::bad_prompt());
    }
    Ok(body.prompt)
}

/// Answer `POST /api/sessions/{id}/prompt`: 202 and the row, now `running`, or ADR-0008's envelope.
///
/// The decisions are in this order, and each one can only be reached by passing the ones above it:
/// the body (`bad_body`), the row (`not_found`), the state (`not_in_state` — one code per state, D2), and
/// the transition itself. The transition is the [table's](crate::sessions::state), applied by the one
/// function that applies it; a lost race — the row moved between the read and the apply — is refused
/// with the code of the state it moved to, so a client never gets a 202 for a session that is not
/// running (D3).
///
/// The row moves **before** the answer and before any event (D4): the body is refetched after the
/// transition, so a client that acts on the 202 finds `running`, and the turn runner is spawned only
/// once the answer is built, so no response waits for a turn.
pub async fn prompt(state: &AppState, id: &str, body: Result<Bytes, BytesRejection>) -> Response {
    let body = match body {
        Ok(body) => body,
        // Every rejection — a body hyper could not buffer, or one over axum's 2 MiB default — is
        // `bad_body`, the same rule the create follows.
        Err(_) => return ApiError::bad_prompt().into_response(),
    };
    let prompt = match parse_prompt(&body) {
        Ok(prompt) => prompt,
        Err(error) => return error.into_response(),
    };
    let current = match one_locked(state, id) {
        Ok(Some(session)) => session,
        Ok(None) => return ApiError::unknown_session().into_response(),
        Err(error) => return error.into_response(),
    };
    // Refused before the transition is asked for, so the code names the state the client can see
    // rather than the state the table happened to find.
    if current.status != Status::Idle {
        return ApiError::not_in_state(current.status, NOT_IDLE_PROMPT).into_response();
    }
    if let Err(error) = apply_prompt(state, id).await {
        return error.into_response();
    }
    // The row is `running` now, so the turn is started **before** anything else can fail: a refetch
    // that failed would otherwise leave a `running` row whose turn never began. The abort ladder
    // (HS-016) can end such a row, which is why this order is defensible rather than merely fast.
    // The row the runner is handed is the one read above with the status the table has just written;
    // the refetch only improves the 202's body.
    let mut running = current;
    running.status = Status::Running;
    running.status_reason = None;
    if let Ok(Some(row)) = one_locked(state, id) {
        running = row;
    }
    state.turner().spawn(running.clone(), prompt);
    (StatusCode::ACCEPTED, Json(running)).into_response()
}

/// Answer `POST /api/sessions/{id}/abort`: 202 and the row as it stands, or ADR-0008's envelope.
///
/// Two decisions, in this order: the row (`not_found`), then the state — only a `running` session
/// has a turn to abort, and every other state is `409 session_<state>`, `idle` included (HS-016,
/// ADR-0020). Nothing is parsed: the request has no body, and a client that sends one is ignored
/// rather than refused, because no field of it could change what an abort does.
///
/// The ladder runs **off** the request's task, so the 202 means "accepted": progress arrives as
/// `session.status` events (spec item 16), and the body is the row as it stands — `running` — not a
/// promise about the state it will hold. A second abort racing the first is refused by the state
/// check, or climbs a ladder whose rungs are both already spent.
pub async fn abort(state: &AppState, id: &str) -> Response {
    let current = match one_locked(state, id) {
        Ok(Some(session)) => session,
        Ok(None) => return ApiError::unknown_session().into_response(),
        Err(error) => return error.into_response(),
    };
    if current.status != Status::Running {
        return ApiError::not_in_state(current.status, NOT_RUNNING_ABORT).into_response();
    }
    // Spawned after the decision and before the answer is built, so no response ever waits for a
    // grace period — and so a ladder that fired instantly still answers the row it was given.
    state.ladder().spawn(current.clone(), ladder::FORCED_ABORT);
    (StatusCode::ACCEPTED, Json(current)).into_response()
}

/// A steer's one field, non-empty after trimming. Anything else — a non-object, a missing or blank
/// `guidance`, a second field — is `bad_body` with the steer's own sentence (ADR-0008).
fn parse_steer(bytes: &[u8]) -> Result<String, ApiError> {
    let body: SteerBody = serde_json::from_slice(bytes).map_err(|_| ApiError::bad_steer())?;
    if body.guidance.trim().is_empty() {
        return Err(ApiError::bad_steer());
    }
    Ok(body.guidance)
}

/// Answer `POST /api/sessions/{id}/steer`: 202 and the row, or ADR-0008's envelope.
///
/// The order is the decision (ADR-0021): the body (`bad_body`), the row (`not_found`), the state
/// (`409 session_<state>` — only a running turn can take guidance), the agent, the record. The agent
/// comes before the record because the transcript must show what was **delivered**, not what was
/// attempted; a backend that cannot deliver the guidance answers `500 internal`, because the client
/// can fix neither the backend nor a turn that stopped underneath it.
///
/// A steer changes no state: the transition table has no row for it, so this handler never asks for
/// one. What it does do is move `last_active_at` — HS-015's acceptance names prompt, settle and steer
/// — and append the `turn.steer` frame the transcript renders.
pub async fn steer(state: &AppState, id: &str, body: Result<Bytes, BytesRejection>) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(_) => return ApiError::bad_steer().into_response(),
    };
    let guidance = match parse_steer(&body) {
        Ok(guidance) => guidance,
        Err(error) => return error.into_response(),
    };
    let current = match one_locked(state, id) {
        Ok(Some(session)) => session,
        Ok(None) => return ApiError::unknown_session().into_response(),
        Err(error) => return error.into_response(),
    };
    if current.status != Status::Running {
        return ApiError::not_in_state(current.status, NOT_RUNNING_STEER).into_response();
    }
    if let Err(error) = state.backend.steer(&current, &guidance).await {
        // The turn may have ended between the read and the call, which is an ordinary race; a
        // backend that does not implement steering is a deployment mistake. Neither is something the
        // client can act on differently, so both are the fixed 500 — and `internal` is what logs the
        // error, once.
        return internal("steer", error).into_response();
    }
    activity::touch(&state.database, &current.id);
    record_steer(state, &current.id, &guidance).await;
    (StatusCode::ACCEPTED, Json(current)).into_response()
}

/// Append the `turn.steer` frame, so the transcript can show what was delivered and when.
///
/// A failed append means the writer has stopped, which in a served process means the process is
/// going down; the loss is logged and the request still answers 202, because the guidance reached the
/// agent and the record of it is the report rather than the act.
async fn record_steer(state: &AppState, id: &str, guidance: &str) {
    let payload = serde_json::json!({ "text": guidance }).to_string();
    let event = NewEvent {
        session_id: id.to_string(),
        kind: TURN_STEER.to_string(),
        payload,
        created_at: clock::unix_now(),
    };
    if let Err(error) = state.events.append(event).await {
        tracing::warn!(
            target: "holosmith::http::sessions",
            session = id,
            %error,
            "a steer's event was not queued"
        );
    }
}

/// Answer `POST /api/sessions/{id}/resume`: 202 and the row, now `starting`, or ADR-0008's envelope.
///
/// The decisions run in this order: the row (`not_found`), the state (the table's two `resume` rows are
/// `stopped` and `error`; everything else is refused with its own code), the project's refusals (HS-026 —
/// the same three codes a create answers), the cap, then the transition.
///
/// **The cap is checked before the transition**, unlike the create's lookup: a resume takes a slot, and
/// refusing after the row moved would leave a session `starting` with nothing provisioning it. Spec
/// item 8 allows either branch — "evicts the least recently active idle session first, **or** returns
/// 409" — and HS-020 gave the eviction to the **create**, which is the request that asks for a new
/// slot; a resume keeps the refusal, so the two answers stay one policy each (ADR-0022, ADR-0024).
///
/// The provisioner does the work, as it does for a create: `stopped -> starting` is the table's, and
/// the four steps are the way back (spec item 8's resume row, item 16's "the same session id reopens
/// the pi session"). Nothing else about the session is touched — the events, the history and the
/// worktree path are what a stop left alone (spec item 15).
pub async fn resume(state: &AppState, id: &str) -> Response {
    let current = match one_locked(state, id) {
        Ok(Some(session)) => session,
        Ok(None) => return ApiError::unknown_session().into_response(),
        Err(error) => return error.into_response(),
    };
    if !matches!(current.status, Status::Stopped | Status::Error) {
        return ApiError::not_in_state(current.status, NOT_RESUMABLE).into_response();
    }
    // The project's refusals are read for a resume too (HS-026): a session created while its project was
    // plain would otherwise be brought back into a mount the create would have refused. Before the cap,
    // for the create's reason — a resume that cannot run must not refuse for a slot it will not take.
    if let Some(refusal) = refuse_project(state, &current.project).await {
        return refusal.into_response();
    }
    match live_at_cap(state) {
        Ok(true) => return ApiError::cap_reached().into_response(),
        Ok(false) => {}
        Err(error) => return error.into_response(),
    }
    if let Err(error) = apply_trigger(state, id, Trigger::Resume, NOT_RESUMABLE, "resume").await {
        return error.into_response();
    }
    // A resume is the operator acting on the session, so the row's activity moves with it: without
    // this, a session stopped a week ago and just resumed would sort as HS-020's least recently active
    // idle row and be evicted first. The touch is the same one a prompt and a steer make.
    activity::touch(&state.database, id);
    // The row is `starting` now, and the provisioner is spawned before the answer is built, so no
    // response waits for a step. The refetch only improves the 202's body.
    let mut starting = current;
    starting.status = Status::Starting;
    starting.status_reason = None;
    starting.stopped_at = None;
    if let Ok(Some(row)) = one_locked(state, id) {
        starting = row;
    }
    state.provisioner().spawn(starting.clone());
    (StatusCode::ACCEPTED, Json(starting)).into_response()
}

/// Move the row with `trigger`, or the refusal to answer.
///
/// The callers that use it — the resume and the delete — differ only in their trigger, the sentence a
/// refusal carries and the operation a 500 names; the four arms underneath are the same decision every
/// time. The table is the authority (ADR-0016) and the handler's earlier read only chose the fast
/// refusal, so a lost race is refused with the code of the state the row *moved to*: a client never
/// gets a 202 for a session that is not in the state the answer promised.
///
/// `Unreported` is `Ok` because the row is already right and only the report of it was lost (a process
/// on its way down); the caller's work must still run, or a `deleting` row would have nothing deleting
/// it and a `starting` one nothing provisioning it.
async fn apply_trigger(
    state: &AppState,
    id: &str,
    trigger: Trigger,
    refusal: &'static str,
    operation: &'static str,
) -> Result<(), ApiError> {
    match state.transitioner().apply(id, trigger, None).await {
        Ok(_) => Ok(()),
        Err(StateError::Unreported(_)) => Ok(()),
        Err(StateError::Illegal { .. }) => match one_locked(state, id) {
            Ok(Some(session)) => Err(ApiError::not_in_state(session.status, refusal)),
            Ok(None) => Err(ApiError::unknown_session()),
            Err(error) => Err(error),
        },
        Err(StateError::UnknownSession(_)) => Err(ApiError::unknown_session()),
        Err(error) => Err(internal(operation, error)),
    }
}

/// Whether the machine already runs the maximum number of live sessions.
///
/// Read under the same `Arc<Mutex<Database>>` the create counts under, but not in the create's one
/// scope: a resume's count and transition are two statements, and the window between them is a window
/// two resumes can share. That is a cap that can be overrun by one, which HS-020's eviction makes
/// uninteresting; refusing before the transition is what keeps a row from being stranded in `starting`.
fn live_at_cap(state: &AppState) -> Result<bool, ApiError> {
    let database = state
        .database
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let live = database
        .sessions()
        .live_count()
        .map_err(|error| internal("live_count", error))?;
    Ok(live >= u64::from(state.config.max_live_sessions))
}

/// Answer `DELETE /api/sessions/{id}`: 202 and the row, now `deleting`, or ADR-0008's envelope.
///
/// The delete flow's request half, in four steps, and the order is the decision (ADR-0023):
///
/// 1. **The row** (`not_found`), and whether it is already `deleting`. A repeat request for a row in
///    that state answers 202 and re-runs the removal, which is how a removal that failed half way is
///    retried; a request after the row is gone is `not_found`. Both are deterministic on the row a
///    client can see.
/// 2. **The turn**, if the session is running: the ladder's [`Ladder::end_turn`](crate::ladder) with
///    the reason `deleted`. The snapshot must not race an agent that is still writing files, and the
///    end is bounded by `abort_grace` — a delete is the one request allowed to wait for a turn, and
///    only because the alternative is discarding work it did not snapshot.
/// 3. **The snapshot**, which is why this endpoint can answer `409 unsaved_work` at all: the row is
///    still whatever step 2 left it, so a refused snapshot leaves nothing half-done. `?force=true`
///    forgives the failure but does not skip the attempt — spec item 9 says the snapshot runs before
///    a delete, and a caller asking to force has already been told it fails.
/// 4. **The transition** (`any + delete → deleting`), the answer, and then the removal off the
///    request's task. From here the client has a `deleting` row and the 202 never waits for a
///    `docker rm`.
///
/// A `deleting` row is the one state no other request in this file accepts, so a delete cannot race
/// a prompt, an abort or a resume: those are refused with `409 session_deleting`.
pub async fn delete(state: &AppState, id: &str, query: RawQuery) -> Response {
    // Read before anything else, so a typo in the query cannot delete work it was not forced to.
    let force = force_requested(query.0.as_deref());
    let mut current = match one_locked(state, id) {
        Ok(Some(session)) => session,
        Ok(None) => return ApiError::unknown_session().into_response(),
        Err(error) => return error.into_response(),
    };
    if current.status == Status::Deleting {
        // Already asked, not yet removed: answer the row the client can see and give the removal
        // another run. No snapshot and no second event — the point of no return was crossed by the
        // request that moved the row here.
        state.deleter().spawn(current.clone());
        return (StatusCode::ACCEPTED, Json(current)).into_response();
    }
    if current.status == Status::Running {
        let ending = state.ladder().end_turn(&current, deleter::DELETED).await;
        tracing::info!(
            target: "holosmith::http::sessions",
            session = id,
            ending = ?ending,
            "ended a running turn so the session could be deleted"
        );
        // The row the snapshot and the backend are handed is the one the turn's end left behind,
        // not the `running` row read above: its reason and its state are what the removal is about.
        if let Ok(Some(row)) = one_locked(state, id) {
            current = row;
        }
    }
    if let Err(error) = state.backend.snapshot(&current).await {
        if !force {
            return ApiError::unsaved_work().into_response();
        }
        // Forced: the operator asked for the work to go anyway. The failure is still the log's,
        // because a snapshot that fails every time is a broken image rather than a decision.
        tracing::warn!(
            target: "holosmith::http::sessions",
            session = id,
            %error,
            "deleting a session past a failed snapshot because the request forced it"
        );
    }
    if let Err(error) = apply_trigger(state, id, Trigger::Delete, NOT_DELETABLE, "delete").await {
        return error.into_response();
    }
    // `deleting` now, and the removal runs off this task: the answer is the row as the table has
    // just written it, and the refetch only makes sure nothing about it is stale.
    let mut deleting = current.clone();
    deleting.status = Status::Deleting;
    deleting.status_reason = None;
    if let Ok(Some(row)) = one_locked(state, id) {
        deleting = row;
    }
    state.deleter().spawn(deleting.clone());
    (StatusCode::ACCEPTED, Json(deleting)).into_response()
}

/// Whether the request carried `force=true`.
///
/// The spec names one spelling, `?force=true`, and this accepts exactly that among the query's
/// pairs. Everything else — no query, a bare `?force`, `?force=1`, `?force=TRUE`, a typo — is not
/// force, so a mistake fails safe with the 409 rather than discarding uncommitted work. There is no
/// error code for an unrecognised query in the spec, and inventing one would make a typo louder
/// than the work it was about to destroy.
fn force_requested(query: Option<&str>) -> bool {
    query
        .unwrap_or_default()
        .split('&')
        .any(|pair| pair == "force=true")
}

/// Move the row to `running`, or the refusal to answer.
///
/// The table is the authority (ADR-0016); the read above only chose the fast refusal. A lost race is
/// retried **once**: if the row is `idle` again, the turn that won has already settled and the
/// prompt is legal now, so a client is not refused for a session that is idle the instant it asks.
/// A row that is gone is `not_found`, and any other state is refused with its own code.
async fn apply_prompt(state: &AppState, id: &str) -> Result<(), ApiError> {
    for _ in 0..2 {
        match state.transitioner().apply(id, Trigger::Prompt, None).await {
            Ok(_) => return Ok(()),
            // The row moved but its event did not reach the writer — a process on its way down.
            // The row is what a client acts on, so this is the prompt succeeding, not failing.
            Err(StateError::Unreported(_)) => return Ok(()),
            Err(StateError::Illegal { .. }) => {}
            Err(StateError::UnknownSession(_)) => return Err(ApiError::unknown_session()),
            Err(error) => return Err(internal("prompt", error)),
        }
        match one_locked(state, id) {
            Ok(Some(session)) if session.status == Status::Idle => continue,
            Ok(Some(session)) => {
                return Err(ApiError::not_in_state(session.status, NOT_IDLE_PROMPT));
            }
            Ok(None) => return Err(ApiError::unknown_session()),
            Err(error) => return Err(error),
        }
    }
    // Two races in a row, the last of which left the row `idle`: a third attempt would be a loop
    // around a state that keeps moving, and no state code would be true. The fixed sentence is the
    // honest answer (ADR-0008).
    Err(ApiError::internal())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::error::{
        BAD_BODY, BAD_IDEMPOTENCY_KEY, BAD_MODEL, BAD_PROJECT, BAD_THINKING,
        MISSING_IDEMPOTENCY_KEY, UNKNOWN_FIELD,
    };

    fn body(project: &str, model: &str, thinking: &str) -> Vec<u8> {
        format!(r#"{{"project":"{project}","model":"{model}","thinking":"{thinking}"}}"#)
            .into_bytes()
    }

    fn key_headers(key: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            KEY_HEADER,
            HeaderValue::from_str(key).expect("a test key is a header value"),
        );
        headers
    }

    fn refusal<T>(result: Result<T, ApiError>) -> ApiError {
        match result {
            Ok(_) => panic!("the request was accepted"),
            Err(error) => error,
        }
    }

    fn assert_code<T>(result: Result<T, ApiError>, code: &'static str) {
        let error = refusal(result);
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{code}");
        assert_eq!(error.body.code, code);
    }

    #[test]
    fn a_project_is_one_path_segment() {
        let longest = "a".repeat(MAX_PROJECT_CHARS);
        for project in ["my.repo_2-x", "a", longest.as_str()] {
            assert!(validate_project(project).is_ok(), "{project:?}");
        }
        let too_long = "a".repeat(MAX_PROJECT_CHARS + 1);
        for project in [
            "",
            ".",
            "..",
            "a/b",
            "a b",
            "proj.ect/..",
            too_long.as_str(),
        ] {
            let error = refusal(validate_project(project));
            assert_eq!(error.body.code, BAD_PROJECT, "{project:?}");
        }
    }

    #[test]
    fn a_model_is_one_model_name() {
        let longest = "a".repeat(MAX_MODEL_CHARS);
        for model in [
            "claude-sonnet-4.5",
            "openrouter/anthropic:beta",
            longest.as_str(),
        ] {
            assert!(validate_model(model).is_ok(), "{model:?}");
        }
        let too_long = "a".repeat(MAX_MODEL_CHARS + 1);
        for model in ["", "claude!", "a b", too_long.as_str()] {
            let error = refusal(validate_model(model));
            assert_eq!(error.body.code, BAD_MODEL, "{model:?}");
        }
    }

    #[test]
    fn a_thinking_level_is_a_lowercase_word_starting_with_a_letter() {
        let longest = "a".repeat(MAX_THINKING_CHARS);
        for thinking in ["high", "x", "x-high_2", longest.as_str()] {
            assert!(validate_thinking(thinking).is_ok(), "{thinking:?}");
        }
        let too_long = "a".repeat(MAX_THINKING_CHARS + 1);
        for thinking in ["", "1high", "High", "high!", too_long.as_str()] {
            let error = refusal(validate_thinking(thinking));
            assert_eq!(error.body.code, BAD_THINKING, "{thinking:?}");
        }
    }

    #[test]
    fn a_well_shaped_body_parses_and_an_absent_network_is_off() {
        let parsed = parse_body(&body("my.repo_2-x", "claude-sonnet-4.5", "high"))
            .expect("the four-field body parses");
        assert_eq!(parsed.project, "my.repo_2-x");
        assert_eq!(parsed.model, "claude-sonnet-4.5");
        assert_eq!(parsed.thinking, "high");
        assert!(!parsed.network, "an absent network is off");
        let online = parse_body(br#"{"project":"p","model":"m","thinking":"high","network":true}"#)
            .expect("the network field parses");
        assert!(online.network);
    }

    #[test]
    fn a_body_that_is_not_a_json_object_is_bad_body() {
        for text in [
            "not json",
            "",
            "[]",
            "null",
            "42",
            r#""project""#,
            r#"{"project":"p","model":"m","thinking":"high""#,
        ] {
            let error = refusal(parse_body(text.as_bytes()));
            assert_eq!(error.body.code, BAD_BODY, "{text:?}");
        }
    }

    #[test]
    fn a_body_of_the_wrong_shape_is_bad_body() {
        for text in [
            "{}",
            r#"{"project":5,"model":"m","thinking":"high"}"#,
            r#"{"project":"p","thinking":"high"}"#,
            r#"{"project":"p","model":"m","thinking":null}"#,
            r#"{"project":"p","model":"m","thinking":"high","network":"yes"}"#,
        ] {
            let error = refusal(parse_body(text.as_bytes()));
            assert_eq!(error.body.code, BAD_BODY, "{text:?}");
        }
    }

    #[test]
    fn a_body_with_a_field_the_endpoint_does_not_accept_is_unknown_field() {
        for text in [
            r#"{"project":"p","model":"m","thinking":"high","extra":1}"#,
            r#"{"project":"p","model":"m","thinking":"high","id":"x"}"#,
        ] {
            let error = refusal(parse_body(text.as_bytes()));
            assert_eq!(error.body.code, UNKNOWN_FIELD, "{text:?}");
        }
    }

    #[test]
    fn a_key_is_one_bounded_non_empty_value() {
        assert_eq!(idempotency_key(&key_headers("k")).expect("one value"), "k");
        let without_key = HeaderMap::new();
        assert_code(
            request_parts(&without_key, &body("p", "m", "high")),
            MISSING_IDEMPOTENCY_KEY,
        );
        let mut empty = HeaderMap::new();
        empty.insert(KEY_HEADER, HeaderValue::from_static(""));
        assert_code(
            request_parts(&empty, &body("p", "m", "high")),
            MISSING_IDEMPOTENCY_KEY,
        );
        let too_long = "k".repeat(MAX_KEY_CHARS + 1);
        assert_code(
            request_parts(&key_headers(&too_long), &body("p", "m", "high")),
            BAD_IDEMPOTENCY_KEY,
        );
        let mut repeated = key_headers("one");
        repeated.append(KEY_HEADER, HeaderValue::from_static("two"));
        assert_code(
            request_parts(&repeated, &body("p", "m", "high")),
            BAD_IDEMPOTENCY_KEY,
        );
        let longest = "k".repeat(MAX_KEY_CHARS);
        assert!(idempotency_key(&key_headers(&longest)).is_ok());
    }

    #[test]
    fn the_precedence_is_body_then_unknown_then_key_then_fields() {
        // Body first: `{}` is missing fields, so it is `bad_body` even with no key at all.
        assert_code(request_parts(&HeaderMap::new(), b"{}"), BAD_BODY);
        // An unknown field is decided before the missing key.
        assert_code(
            request_parts(
                &HeaderMap::new(),
                br#"{"project":"p","model":"m","thinking":"high","extra":1}"#,
            ),
            UNKNOWN_FIELD,
        );
        // The key is decided before the fields: this body's project is bad and it has no key.
        assert_code(
            request_parts(&HeaderMap::new(), &body("..", "m", "high")),
            MISSING_IDEMPOTENCY_KEY,
        );
        // The fields last.
        assert_code(
            request_parts(&key_headers("k"), &body("..", "m", "high")),
            BAD_PROJECT,
        );
    }

    /// HS-017: a steer's body is one non-empty `guidance`, and the sentence it is refused with is
    /// its own — the code is shared with a prompt's `bad_body`, which is the point of ADR-0008.
    #[test]
    fn a_steer_body_is_one_non_empty_guidance_and_nothing_else() {
        assert_eq!(
            parse_steer(br#"{"guidance":"and the lexer too"}"#).expect("the body parses"),
            "and the lexer too"
        );
        for text in [
            "",
            "not json",
            "[]",
            "null",
            "42",
            "{}",
            r#"{"guidance":""}"#,
            r#"{"guidance":"   "}"#,
            r#"{"guidance":42}"#,
            r#"{"guidance":null}"#,
            r#"{"guidance":"g","extra":1}"#,
            r#"{"text":"a steer"}"#,
        ] {
            let error = refusal(parse_steer(text.as_bytes()));
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{text:?}");
            assert_eq!(error.body.code, BAD_BODY, "{text:?}");
            assert!(
                error.body.message.contains("guidance"),
                "the sentence must name the steer's own field: {text:?}"
            );
        }
    }

    /// HS-019: a delete's only query is `force=true`, and everything else fails safe.
    #[test]
    fn force_is_the_literal_force_true_and_nothing_else() {
        for query in [
            "force=true",
            "force=true&x=y",
            "x=y&force=true",
            // A repeated parameter is a client bug either way; one `force=true` among them is the
            // caller saying it, which is what the matcher reads.
            "force=true&force=false",
        ] {
            assert!(force_requested(Some(query)), "{query:?}");
        }
        // A typo must not delete uncommitted work: `?force`, `?force=1` and a case variant are all
        // "not forced", so the answer is the 409 rather than the removal.
        for query in [
            None,
            Some(""),
            Some("force"),
            Some("force="),
            Some("force=false"),
            Some("force=1"),
            Some("force=TRUE"),
            Some("Force=true"),
            Some("FORCE=true"),
            Some("force=truex"),
            Some("notforce=true"),
            Some("force=false&force=false"),
        ] {
            assert!(!force_requested(query), "{query:?}");
        }
    }

    #[test]
    fn a_prompt_body_is_one_non_empty_prompt_and_nothing_else() {
        assert_eq!(
            parse_prompt(br#"{"prompt":"fix the parser"}"#).expect("the body parses"),
            "fix the parser"
        );
        for text in [
            "",
            "not json",
            "[]",
            "null",
            "42",
            "{}",
            r#"{"prompt":""}"#,
            r#"{"prompt":42}"#,
            r#"{"prompt":null}"#,
            r#"{"prompt":"p","extra":1}"#,
            r#"{"prompt":"p","model":"m"}"#,
        ] {
            let error = refusal(parse_prompt(text.as_bytes()));
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{text:?}");
            assert_eq!(error.body.code, BAD_BODY, "{text:?}");
        }
    }

    #[test]
    fn the_response_is_the_fifteen_column_names_plus_events() {
        let session = Session {
            id: "0123456789abcdef".to_string(),
            project: "holosmith".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            thinking: "high".to_string(),
            worktree: String::new(),
            base_commit: None,
            status: crate::sessions::Status::Starting,
            status_reason: None,
            client_key: Some("the-key".to_string()),
            network: false,
            created_at: 1_760_000_000,
            last_active_at: 1_760_000_000,
            stopped_at: None,
            usage_in: 0,
            usage_out: 0,
        };
        let created = CreatedSession::of(session.clone());
        let json = serde_json::to_string(&created).expect("the response serialises");
        let value: Value = serde_json::from_str(&json).expect("the response is JSON");
        let object = value.as_object().expect("the response is an object");
        assert_eq!(
            object.len(),
            16,
            "the response is the row plus one key: {json}"
        );
        assert_eq!(object["id"].as_str(), Some(session.id.as_str()));
        assert_eq!(object["status"].as_str(), Some("starting"));
        assert_eq!(object["network"].as_bool(), Some(false));
        assert_eq!(object["base_commit"].as_str(), None);
        assert_eq!(
            object["events"].as_str(),
            Some(format!("/api/sessions/{}/events", session.id).as_str())
        );
        // The row's keys keep column order, and `events` is last.
        let mut at = 0;
        for name in [
            "id",
            "project",
            "model",
            "thinking",
            "worktree",
            "base_commit",
            "status",
            "status_reason",
            "client_key",
            "network",
            "created_at",
            "last_active_at",
            "stopped_at",
            "usage_in",
            "usage_out",
            "events",
        ] {
            let needle = format!("\"{name}\":");
            let found = json[at..]
                .find(&needle)
                .unwrap_or_else(|| panic!("{name} is missing or out of order in {json}"));
            at += found + needle.len();
        }
    }
}
