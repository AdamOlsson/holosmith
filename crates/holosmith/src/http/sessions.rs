//! The sessions collection: `POST /api/sessions` validates the body, writes one `starting` row and
//! answers 202 (HS-008, ADR-0012); `GET /api/sessions` answers every row in the repository's own
//! order together with the machine's network default (HS-013, ADR-0017); `GET /api/sessions/{id}`
//! answers one row, which is what the session screen opens with (HS-014); and
//! `POST /api/sessions/{id}/prompt` starts a turn and answers 202 with the now-`running` row
//! (HS-015); `POST /api/sessions/{id}/abort` accepts an abort and answers 202 with the row while the
//! ladder ends the turn off-request (HS-016).
//!
//! The body is parsed by hand rather than through `axum::Json<T>`: that extractor rejects a bad
//! body with its own `text/plain` answer, which would break ADR-0008's envelope on exactly the
//! 422 path this endpoint exists to get right. The handler also never holds the database's
//! `MutexGuard` across an `.await`; the lock scope is one synchronous block, and the compiler
//! enforces it because a guard is not `Send`.

use std::sync::PoisonError;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::clock;
use crate::http::app::AppState;
use crate::http::error::{ApiError, internal};
use crate::ladder;
use crate::sessions::state::{StateError, Trigger};
use crate::sessions::{NewSession, RepoError, Session, Status};

/// The header a create's idempotency key travels in.
const KEY_HEADER: &str = "idempotency-key";

/// What a prompt is refused with: the sentence names the request, because an envelope that reads
/// like another endpoint's is how a client builder picks the wrong branch.
const NOT_IDLE_PROMPT: &str = "the session is not idle, so it cannot be prompted";

/// What an abort is refused with. `abort` is the one request that needs a *running* turn, so a
/// session in any other state is refused rather than answered with a 202 that does nothing
/// (spec item 16: abort stops a turn, and there is no turn otherwise).
const NOT_RUNNING_ABORT: &str = "the session is not running, so it has no turn to abort";

/// How many characters a key may hold. Generous for a UUID, small enough that the unique column
/// cannot become a payload.
const MAX_KEY_CHARS: usize = 200;

/// The body's fields, exactly: any other key is `unknown_field`.
const FIELDS: [&str; 4] = ["project", "model", "thinking", "network"];

/// A prompt's body: one field, exactly, and a body that is anything else is `bad_body` (D1). No
/// length cap beyond axum's own 2 MiB: the spec sets none, and a cap would be an invented number.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptBody {
    prompt: String,
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
    match create_locked(state, &parsed, key) {
        Ok((session, inserted)) => {
            // The handoff, and the only place this story spawns anything. The answer is built
            // first, from the `starting` row this call just wrote, and only then does the
            // provisioner get the session on another task — so the 202 can never wait for a step,
            // and the body can never show a state a step has already moved past. A retry of the
            // same key inserts nothing and is answered from the row it already has, so one
            // session is provisioned once.
            let response = created(session.clone());
            if inserted {
                state.provisioner().spawn(session);
            }
            response
        }
        Err(error) => error.into_response(),
    }
}

/// The one lock scope: lookup, cap count and insert, with no `.await` inside so the guard cannot
/// outlive a synchronous statement. `live_count` and `create` share it, so two creates cannot
/// both see a free slot (ADR-0012).
///
/// `true` as the second element means this call inserted the row, so it is the caller's to
/// provision; `false` means the idempotency key already named a session.
fn create_locked(
    state: &AppState,
    body: &CreateBody,
    key: &str,
) -> Result<(Session, bool), ApiError> {
    let database = state
        .database
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let repository = database.sessions();
    // The lookup runs before the cap: a retry whose first attempt filled the last slot must get
    // its own session, not 409.
    if let Some(existing) = repository
        .by_client_key(key)
        .map_err(|error| internal("by_client_key", error))?
    {
        return Ok((existing, false));
    }
    let live = repository
        .live_count()
        .map_err(|error| internal("live_count", error))?;
    if live >= u64::from(state.config.max_live_sessions) {
        return Err(ApiError::cap_reached());
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
        Ok(session) => Ok((session, true)),
        Err(RepoError::DuplicateClientKey) => match repository.by_client_key(key) {
            Ok(Some(winner)) => Ok((winner, false)),
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
        Ok(None) => ApiError::not_found().into_response(),
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
        Ok(None) => return ApiError::not_found().into_response(),
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
    // The row is `running` now, so the turn is started **before** anything else can fail: a session
    // left in `running` with no runner has no way out until HS-016 adds abort, and a refetch that
    // failed would otherwise strand exactly that. The row the runner is handed is the one read
    // above with the status the table has just written; the refetch only improves the 202's body.
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
        Ok(None) => return ApiError::not_found().into_response(),
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
            Err(StateError::UnknownSession(_)) => return Err(ApiError::not_found()),
            Err(error) => return Err(internal("prompt", error)),
        }
        match one_locked(state, id) {
            Ok(Some(session)) if session.status == Status::Idle => continue,
            Ok(Some(session)) => {
                return Err(ApiError::not_in_state(session.status, NOT_IDLE_PROMPT));
            }
            Ok(None) => return Err(ApiError::not_found()),
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
