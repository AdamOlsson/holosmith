//! One JSON error envelope for every error that has a body (ADR-0008).
//!
//! `{"code": "<machine code>", "message": "<one human sentence>"}`. The code is a stable
//! snake_case name from the spec, never the transport status; the message may be rewritten
//! without breaking a client.

use std::fmt::Display;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// An unknown path under `/api/`.
pub const NOT_FOUND: &str = "not_found";

/// A request's query names no place in the log it can answer from (HS-061).
///
/// The first query parameter this API refuses. A delete's `?force=true` is read leniently because its
/// absence is legal and a typo fails safe; a history page's `before` has no safe default, because
/// "the newest page" is what the stream already gave the client.
pub const BAD_QUERY: &str = "bad_query";

/// The `Host` header is not one this machine answers (ADR-0010).
pub const BAD_HOST: &str = "bad_host";

/// A request that changes something did not come from the control origin (ADR-0010).
pub const BAD_ORIGIN: &str = "bad_origin";

/// A request that changes something is not the Holosmith client (ADR-0010).
pub const BAD_CLIENT: &str = "bad_client";

/// The tailnet user behind the request is not the configured owner (ADR-0010).
pub const BAD_OWNER: &str = "bad_owner";

/// A create's body is not a JSON object with the four accepted fields (HS-008).
pub const BAD_BODY: &str = "bad_body";

/// A create's body carries a field the endpoint does not accept (HS-008).
pub const UNKNOWN_FIELD: &str = "unknown_field";

/// A create did not carry an `Idempotency-Key` header (HS-008).
pub const MISSING_IDEMPOTENCY_KEY: &str = "missing_idempotency_key";

/// A create's `Idempotency-Key` is not one value of at most 200 characters (HS-008).
pub const BAD_IDEMPOTENCY_KEY: &str = "bad_idempotency_key";

/// A create's `project` is not one path segment (HS-008).
pub const BAD_PROJECT: &str = "bad_project";

/// A create's `model` is not one model name (HS-008).
pub const BAD_MODEL: &str = "bad_model";

/// A create's `thinking` is not one thinking level (HS-008).
pub const BAD_THINKING: &str = "bad_thinking";

/// A create named a project whose `.git` is not a directory (HS-026, spec item 1).
pub const GIT_NOT_DIRECTORY: &str = "git_not_directory";

/// A create named a project that has submodules (HS-026, spec item 1).
pub const SUBMODULES_PRESENT: &str = "submodules_present";

/// A create named a project with `extensions.worktreeConfig` enabled (HS-026, spec item 1).
pub const WORKTREE_CONFIG_ENABLED: &str = "worktree_config_enabled";

/// A create was refused because the machine already runs the maximum number of live sessions
/// (HS-008; HS-020 refines the branch with eviction).
pub const CAP_REACHED: &str = "cap_reached";

/// A create could not be completed for a reason no client can fix (HS-008).
pub const INTERNAL: &str = "internal";

/// A prompt was refused because the session is still provisioning (HS-015).
pub const SESSION_STARTING: &str = "session_starting";

/// A prompt was refused because a turn is already running (HS-015; spec item 12).
pub const SESSION_RUNNING: &str = "session_running";

/// A request was refused because the session is stopped (HS-015; spec item 8).
pub const SESSION_STOPPED: &str = "session_stopped";

/// A request was refused because the session is in `error` (HS-015).
pub const SESSION_ERROR: &str = "session_error";

/// A request was refused because the session is being deleted (HS-015).
pub const SESSION_DELETING: &str = "session_deleting";

/// An abort was refused because there is no turn to abort (HS-016; HS-017 names it).
pub const SESSION_IDLE: &str = "session_idle";

/// A delete was refused because the session's uncommitted work could not be snapshotted, so removing
/// it would lose work no client asked to lose (HS-019, spec item 9).
pub const UNSAVED_WORK: &str = "unsaved_work";

/// The machine is draining for a restart, so it starts no new session and no new turn (HS-051, spec item 10
/// step 3).
///
/// The family's first non-4xx code, and deliberately not a `409`: a state refusal says "this session cannot take
/// that request", while this one says "this *machine* cannot take it right now, and the operator is the reason".
pub const DRAINING: &str = "draining";

/// A changes view was refused because there is no worktree to read (HS-052, spec item 12).
///
/// One code for four facts — the row never recorded a worktree, the directory is gone, git no longer registers
/// it, the branch or the project's repository is gone — because the client's move is one (show the sentence)
/// while the operator's next step is the sentence's. A repository git *cannot read* is not this code: that is
/// `internal`, the project refusals' own split between a state and a machine problem.
pub const NO_WORKTREE: &str = "no_worktree";

/// A merge named a target that is not one this machine will merge into (HS-054, spec item 12's rule 6).
///
/// Two facts share it — a name the `merge.targets` config does not list, and a listed name the repository has no
/// branch for — for the reason `cap_reached` has two sentences: the client's move is one (send a target this
/// whole machine and project agree on) while the operator's next step is the sentence's.
pub const BAD_TARGET: &str = "bad_target";

/// A merge named a `strategy` that is not one of spec item 12's three words (HS-054).
///
/// The sibling of `bad_thinking` and `bad_model`: a field whose value is not in the contract's vocabulary is a
/// malformed request, not a state and not a machine problem.
pub const BAD_STRATEGY: &str = "bad_strategy";

/// A merge asked for a field this build does not implement yet (HS-054).
///
/// `merge` and `squash` (HS-055) and `delete_session` (HS-056) are in the contract and not in this build, so a
/// request that names one is refused with a sentence saying so rather than accepted and quietly not done — the
/// one thing an endpoint must never do to a request that says "delete the session".
pub const UNSUPPORTED: &str = "unsupported";

/// A merge could not fast-forward because the target has moved (HS-054, spec item 12's rule 2).
///
/// The spec's own word. One fact covers both shapes rule 2 and rule 4 describe: a target that is ahead of the
/// branch, and a target that advanced between the merge's read and its compare-and-swap.
pub const NOT_FAST_FORWARD: &str = "not_fast_forward";

/// A merge could not fast-forward because the checkout that holds the target has local work (HS-054, rule 4).
///
/// The spec's own word. It is not the session's `dirty` (rule 1's `dirty_worktree`, HS-056): this is the
/// operator's own working tree in the project checkout, and the next step is to commit or stash there.
pub const TARGET_DIRTY: &str = "target_dirty";

/// A merge found the target checked out somewhere that is not the project's own checkout (HS-054).
///
/// The third answer to rule 4's two-arm question. In practice it is a session's worktree, where a container is
/// working: writing that working tree from the host would move files under a running agent, so the merge refuses
/// and says what to change.
pub const TARGET_CHECKED_OUT: &str = "target_checked_out";

/// A merge that could not reconcile two branches (HS-055, spec item 12's rule 3).
///
/// The only code whose response carries a **second payload key**: rule 3 says the call returns "409 `conflict`
/// with the conflicting paths", and the envelope's message is a fixed sentence (ADR-0008), so the paths travel
/// in `conflicts` and the sentence names the next step. ADR-0052's additive `kept` on the delete's 202 is the
/// same shape one route over.
pub const CONFLICT: &str = "conflict";

/// The body of every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: &'static str,
    /// The paths a conflict could not merge, and `None` for every other error — skipped on the wire, so every
    /// other error's body is byte-identical to what it was before this key existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicts: Option<Vec<String>>,
}

/// A status and its envelope, ready to answer with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: ErrorBody,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            body: ErrorBody {
                code,
                message,
                conflicts: None,
            },
        }
    }

    /// What an unknown path under `/api/` answers.
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, NOT_FOUND, "no such endpoint")
    }

    /// What a request naming a session the table does not hold answers (HS-019).
    ///
    /// The same code — the spec names one `not_found` — and a sentence about the *request*: after a
    /// delete, `GET /api/sessions/{id}` is a route that exists and a row that does not, so answering
    /// "no such endpoint" described the wrong thing. ADR-0008 keeps the code stable and the sentence
    /// free for exactly this (the same lesson HS-015's prompt learned from its own demo).
    pub fn unknown_session() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            NOT_FOUND,
            "there is no session with that id",
        )
    }

    /// What a request whose query names no place in the log answers (HS-061).
    ///
    /// The sentence is the caller's, for [`Self::not_in_state`]'s reason: a bad `before` and a bad
    /// `limit` are the same code and two different mistakes, and a caller reading one sentence about the
    /// other has to guess which parameter it got wrong.
    pub fn bad_query(message: &'static str) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, BAD_QUERY, message)
    }

    /// What a request whose `Host` is not in the allow-list answers.
    pub fn bad_host() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            BAD_HOST,
            "the Host header is not one this machine answers",
        )
    }

    /// What a non-`GET` request from another origin answers.
    pub fn bad_origin() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            BAD_ORIGIN,
            "a request that changes something must come from the control origin",
        )
    }

    /// What a non-`GET` request without the JSON client header answers.
    pub fn bad_client() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            BAD_CLIENT,
            "a request that changes something must be the Holosmith client",
        )
    }

    /// What a request from a tailnet user who is not the owner answers.
    pub fn bad_owner() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            BAD_OWNER,
            "this tailnet user is not the configured owner",
        )
    }

    /// What a create whose body is not the expected JSON object answers.
    pub fn bad_body() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_BODY,
            "the body must be a JSON object with `project`, `model`, `thinking` and optionally \
             `network`",
        )
    }

    /// The same code as [`Self::bad_body`], with the sentence a prompt's body deserves.
    ///
    /// The code is the API and the sentence is prose (ADR-0008), so a second answer to "this body
    /// is not a prompt" is a message, not a code: a client that told the operator the create's
    /// fields would be describing the wrong request.
    pub fn bad_prompt() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_BODY,
            "the body must be a JSON object with a non-empty `prompt` string",
        )
    }

    /// The same code as [`Self::bad_body`], with the sentence a steer's body deserves (HS-017).
    ///
    /// A steer carries one `guidance` string and nothing else, so telling the operator about a
    /// prompt's or a create's fields would describe the wrong request (ADR-0008).
    pub fn bad_steer() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_BODY,
            "the body must be a JSON object with a non-empty `guidance` string",
        )
    }

    /// What a create naming a field the endpoint does not accept answers.
    pub fn unknown_field() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            UNKNOWN_FIELD,
            "the body carries a field this endpoint does not accept",
        )
    }

    /// What a create without an `Idempotency-Key` header answers.
    pub fn missing_idempotency_key() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            MISSING_IDEMPOTENCY_KEY,
            "a create must carry an `Idempotency-Key` header",
        )
    }

    /// What a create whose `Idempotency-Key` is not one bounded value answers.
    pub fn bad_idempotency_key() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_IDEMPOTENCY_KEY,
            "the `Idempotency-Key` header must be one value of at most 200 characters",
        )
    }

    /// What a create whose `project` is not one path segment answers.
    pub fn bad_project() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_PROJECT,
            "`project` must be one path segment of letters, digits, `.`, `_` and `-`, at most \
             255 characters, and not `.` or `..`",
        )
    }

    /// What a create whose `model` is not one model name answers.
    pub fn bad_model() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_MODEL,
            "`model` must be 1–128 characters of letters, digits, `.`, `:`, `/` and `-`",
        )
    }

    /// What a create whose `thinking` is not one thinking level answers.
    pub fn bad_thinking() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_THINKING,
            "`thinking` must be 1–32 lowercase letters, digits, `_` and `-`, starting with a \
             letter",
        )
    }

    /// What a create naming a project whose `.git` is not a directory answers (HS-026, spec item 1).
    ///
    /// A sentence each rather than one shared sentence, because the operator's next step depends on
    /// which hole was found: a checkout that is not a plain one, submodules whose hooks would live
    /// inside the mount, or a per-worktree config the agent could write. The page prints the sentence alone
    /// (`createFailure` in `web/src/transcript.ts`): the story asks for an explanation, not a code.
    pub fn git_not_directory() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            GIT_NOT_DIRECTORY,
            "the project's .git is not a directory, so it was refused: a session needs a plain \
             checkout, not a linked worktree, a submodule or a missing repository",
        )
    }

    /// What a create naming a project with submodules answers (HS-026, spec item 1).
    pub fn submodules_present() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            SUBMODULES_PRESENT,
            "the project has submodules, whose hooks would live under .git/modules inside the \
             session's mount, so it was refused",
        )
    }

    /// What a create naming a project with `extensions.worktreeConfig` answers (HS-026, spec item 1).
    pub fn worktree_config_enabled() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            WORKTREE_CONFIG_ENABLED,
            "the project enables extensions.worktreeConfig, which would let the agent write a \
             per-worktree config, so it was refused",
        )
    }

    /// What a delete answers when the session's work could not be snapshotted (HS-019).
    ///
    /// The sentence names the escape hatch, because the operator's next tap is the one that has to
    /// carry `?force=true`: an envelope that said only "it did not work" would leave a mis-tap
    /// with no way forward, and the spec's whole point here is that the *second* answer destroys
    /// work deliberately.
    pub fn unsaved_work() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            UNSAVED_WORK,
            "the session's uncommitted work could not be snapshotted, so the session was not \
             deleted; repeat the delete with `?force=true` to discard it",
        )
    }

    /// What a request that names a session with no worktree to read answers (HS-052).
    ///
    /// The **message** is the caller's for [`Self::not_in_state`]'s reason: the four sentences describe four
    /// different next steps (resume the session, restore the project's checkout), and a caller reading one
    /// about another would have to guess which fact it hit. `409` rather than `404`: the session exists and the
    /// route exists — the thing the view would read is not there.
    pub fn no_worktree(message: &'static str) -> Self {
        Self::new(StatusCode::CONFLICT, NO_WORKTREE, message)
    }

    /// What a merge whose body is not `{target, strategy?, message?, delete_session?}` answers (HS-054).
    ///
    /// The same code as [`Self::bad_body`] with this request's own fields, for the reason
    /// [`Self::bad_prompt`] has the prompt's: a client told about a create's fields would be describing the
    /// wrong request (ADR-0008).
    pub fn bad_merge_body() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_BODY,
            "the body must be a JSON object with a `target` string, and optionally `strategy`, `message` and \
             `delete_session`",
        )
    }

    /// What a merge naming a target this machine will not merge into answers (HS-054, rule 6).
    ///
    /// The **message** is the caller's, like [`Self::no_worktree`]'s: a name the config does not list and a
    /// listed name the repository has no branch for are one code and two next steps.
    pub fn bad_target(message: &'static str) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, BAD_TARGET, message)
    }

    /// What a merge naming a `strategy` that is not one of the spec's three words answers (HS-054).
    pub fn bad_strategy(message: &'static str) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, BAD_STRATEGY, message)
    }

    /// What a merge naming a field this build does not implement answers (HS-054).
    ///
    /// The **message** is the caller's for [`Self::not_in_state`]'s reason: `strategy: "squash"` and
    /// `delete_session: true` are two requests whose next steps differ, and a sentence about one read on the
    /// other would be a guess.
    pub fn unsupported(message: &'static str) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, UNSUPPORTED, message)
    }

    /// What a merge that cannot fast-forward answers (HS-054, rule 2).
    pub fn not_fast_forward(message: &'static str) -> Self {
        Self::new(StatusCode::CONFLICT, NOT_FAST_FORWARD, message)
    }

    /// What a merge whose target checkout holds local work answers (HS-054, rule 4).
    pub fn target_dirty(message: &'static str) -> Self {
        Self::new(StatusCode::CONFLICT, TARGET_DIRTY, message)
    }

    /// What a merge whose target is checked out outside the project's own checkout answers (HS-054).
    pub fn target_checked_out(message: &'static str) -> Self {
        Self::new(StatusCode::CONFLICT, TARGET_CHECKED_OUT, message)
    }

    /// What a merge that conflicted answers: rule 3's `409`, with the paths.
    ///
    /// The sentence is fixed, and it describes **this** request's next step — the spec's own "merge the target
    /// into the session's branch and fix the conflicts; then retry", which is also what the Changes sheet's
    /// Resolve-with-agent affordance prompts (HS-058). The paths are the payload.
    pub fn conflict(conflicts: Vec<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: ErrorBody {
                code: CONFLICT,
                message: "the merge conflicts, so nothing was merged; merge the target into the session's \
                          branch and resolve the conflicts there, then merge again",
                conflicts: Some(conflicts),
            },
        }
    }

    /// What a resume at the cap answers (HS-018, spec item 8's "or returns 409").
    ///
    /// The create no longer uses this: since HS-020 it evicts an idle session instead, and answers
    /// [`Self::cap_without_idle`] only when there is nothing to evict (ADR-0024). Two requests, two
    /// next steps, so two sentences on one code.
    pub fn cap_reached() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            CAP_REACHED,
            "this machine already runs the maximum number of live sessions",
        )
    }

    /// What a create at the cap answers when there is nothing to evict (HS-020).
    ///
    /// The same code and the create's own sentence: since the machine evicts an idle session instead
    /// of refusing, a caller that got the refusal is owed the reason it could not — a `running` or
    /// `starting` session is live and not evictable, so the operator's next move is to wait for the
    /// turn rather than to tap again. A resume keeps [`Self::cap_reached`]'s shorter sentence, because
    /// it is a different request with a different next step (ADR-0022, ADR-0024).
    pub fn cap_without_idle() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            CAP_REACHED,
            "this machine already runs the maximum number of live sessions and none of them is \
             idle, so there is no session to stop",
        )
    }

    /// What a create that failed for an unplanned reason answers.
    ///
    /// The message is one fixed sentence: a rusqlite error can carry the database path, so it
    /// goes to the log rather than to a client (ADR-0012).
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            INTERNAL,
            "the server could not complete the request",
        )
    }

    /// What a create, a prompt or a resume answers while the machine drains for a restart (HS-051).
    ///
    /// `503` because the machine is temporarily unable to do what the request asks and the operator is the one
    /// who decides when it can again — not a `409`, which would say the *session* is in the wrong state, and not a
    /// `5xx` from the `internal` family, which would say nothing can be done about it. The sentence names the
    /// drain and the wait it is running, because those are the two things the operator needs to know: what is
    /// happening, and where to read when it is over.
    pub fn draining() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            DRAINING,
            "this machine is draining for a restart, so it starts no new session and no new turn until the \
             drain finishes; `holosmith drain --wait` says when it has",
        )
    }

    /// What a request is refused with when the session's state does not accept it (HS-015, D2;
    /// HS-016).
    ///
    /// One code per state, the state's own machine word: the spec names `session_stopped` (item 8)
    /// and `session_running` (item 12), HS-016 adds `session_idle` (an abort with no turn to abort)
    /// and HS-017 names it too (a steer with no turn). The family is the rule rather than a list of the codes the spec
    /// happens to spell, so `idle` is a member like the rest and there is no umbrella code.
    ///
    /// The **message** is the caller's, because it has to describe its own request: an abort's
    /// refusal must not read like a prompt's (HS-015's demo caught the prompt reusing the create's
    /// sentence). Every caller passes a fixed sentence, which is why the envelope can keep its
    /// borrowed `&'static str`.
    pub fn not_in_state(status: crate::sessions::Status, message: &'static str) -> Self {
        use crate::sessions::Status;
        let code = match status {
            Status::Starting => SESSION_STARTING,
            Status::Idle => SESSION_IDLE,
            Status::Running => SESSION_RUNNING,
            Status::Stopped => SESSION_STOPPED,
            Status::Error => SESSION_ERROR,
            Status::Deleting => SESSION_DELETING,
        };
        Self::new(StatusCode::CONFLICT, code, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// The 500 every handler answers for a failure no client can fix: the operation and the error go
/// to the log, because a rusqlite message can carry the database path, and the client gets the one
/// fixed sentence.
pub fn internal(operation: &'static str, error: impl Display) -> ApiError {
    tracing::error!(
        target: "holosmith::http",
        operation,
        %error,
        "a request could not be completed"
    );
    ApiError::internal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_session_is_the_same_code_with_its_own_sentence() {
        // One code (the spec's), two sentences: an unknown *path* is not an unknown *session*, and a
        // client after a delete reads the second one.
        let error = ApiError::unknown_session();
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.body.code, NOT_FOUND);
        assert_ne!(error.body.message, ApiError::not_found().body.message);
        assert!(
            error.body.message.contains("session"),
            "the sentence does not describe the request: {}",
            error.body.message
        );
    }

    #[test]
    fn the_envelope_is_two_keys_in_order() {
        assert_eq!(
            serde_json::to_string(&ApiError::not_found().body)
                .expect("two string fields always serialise"),
            r#"{"code":"not_found","message":"no such endpoint"}"#
        );
    }

    #[test]
    fn not_found_is_a_404_with_the_specs_code() {
        let error = ApiError::not_found();
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.body.code, NOT_FOUND);
    }

    #[test]
    fn the_response_is_json() {
        let response = ApiError::not_found().into_response();
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    fn assert_refusal(error: ApiError, code: &'static str) {
        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.body.code, code);
        assert_eq!(
            error
                .into_response()
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn bad_host_is_a_403_with_its_code() {
        assert_refusal(ApiError::bad_host(), BAD_HOST);
    }

    #[test]
    fn bad_origin_is_a_403_with_its_code() {
        assert_refusal(ApiError::bad_origin(), BAD_ORIGIN);
    }

    #[test]
    fn bad_client_is_a_403_with_its_code() {
        assert_refusal(ApiError::bad_client(), BAD_CLIENT);
    }

    #[test]
    fn bad_owner_is_a_403_with_its_code() {
        assert_refusal(ApiError::bad_owner(), BAD_OWNER);
    }

    fn assert_answer(error: ApiError, status: StatusCode, code: &'static str) {
        assert_eq!(error.status, status);
        assert_eq!(error.body.code, code);
        assert_eq!(
            error
                .into_response()
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn bad_body_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_body(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_BODY,
        );
    }

    #[test]
    fn unsaved_work_is_a_409_that_names_the_way_past_it() {
        // The code is the spec's (item 9) and the sentence is the operator's only pointer to
        // `?force=true`: a message that named no escape hatch would leave a refused delete with
        // nothing to do next, and the spec's whole point is that the second answer discards work
        // deliberately.
        let error = ApiError::unsaved_work();
        assert_eq!(error.body.code, UNSAVED_WORK);
        assert!(
            error.body.message.contains("?force=true"),
            "the message does not name the escape hatch: {}",
            error.body.message
        );
        assert_answer(error, StatusCode::CONFLICT, UNSAVED_WORK);
    }

    #[test]
    fn unknown_field_is_a_422_with_its_code() {
        assert_answer(
            ApiError::unknown_field(),
            StatusCode::UNPROCESSABLE_ENTITY,
            UNKNOWN_FIELD,
        );
    }

    #[test]
    fn missing_idempotency_key_is_a_422_with_its_code() {
        assert_answer(
            ApiError::missing_idempotency_key(),
            StatusCode::UNPROCESSABLE_ENTITY,
            MISSING_IDEMPOTENCY_KEY,
        );
    }

    #[test]
    fn bad_idempotency_key_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_idempotency_key(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_IDEMPOTENCY_KEY,
        );
    }

    #[test]
    fn bad_project_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_project(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_PROJECT,
        );
    }

    #[test]
    fn bad_model_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_model(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_MODEL,
        );
    }

    #[test]
    fn every_project_refusal_is_a_422_with_its_own_code_and_its_own_sentence() {
        // HS-026's three refusals. Three codes because the operator's next step differs per hole, and a
        // sentence each because the page prints the sentence alone: a shared sentence would make the
        // codes indistinguishable to the human even though the machine can tell them apart.
        let mut sentences = std::collections::BTreeSet::new();
        for (error, code) in [
            (ApiError::git_not_directory(), GIT_NOT_DIRECTORY),
            (ApiError::submodules_present(), SUBMODULES_PRESENT),
            (ApiError::worktree_config_enabled(), WORKTREE_CONFIG_ENABLED),
        ] {
            assert!(
                !error.body.message.contains(code),
                "a message repeats its code: {}",
                error.body.message
            );
            sentences.insert(error.body.message);
            assert_answer(error, StatusCode::UNPROCESSABLE_ENTITY, code);
        }
        assert_eq!(sentences.len(), 3, "two project refusals share a sentence");
    }

    #[test]
    fn a_project_refusal_is_not_the_code_a_bad_project_name_answers() {
        // `bad_project` is the request's shape and `git_not_directory` is the repository's: one is fixed
        // by correcting what the client sent, the other by fixing the checkout on the machine.
        assert_ne!(ApiError::bad_project().body.code, GIT_NOT_DIRECTORY);
        assert_ne!(
            ApiError::bad_project().body.message,
            ApiError::git_not_directory().body.message
        );
    }

    #[test]
    fn bad_thinking_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_thinking(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_THINKING,
        );
    }

    #[test]
    fn cap_reached_is_a_409_with_its_code() {
        assert_answer(ApiError::cap_reached(), StatusCode::CONFLICT, CAP_REACHED);
    }

    #[test]
    fn internal_is_a_500_with_its_code() {
        assert_answer(
            ApiError::internal(),
            StatusCode::INTERNAL_SERVER_ERROR,
            INTERNAL,
        );
    }

    #[test]
    fn a_request_a_state_refuses_is_a_409_naming_that_state() {
        // One code per state, and the code is the state's own machine word: `Status::as_str` is
        // where the word lives, so the constants are proved against it rather than restated.
        use crate::sessions::Status;
        for status in Status::ALL {
            let error = ApiError::not_in_state(status, "the session is not running");
            assert_eq!(error.status, StatusCode::CONFLICT, "{status}");
            assert_eq!(
                error.body.code,
                format!("session_{}", status.as_str()),
                "{status}"
            );
            assert_eq!(error.body.message, "the session is not running", "{status}");
            assert_eq!(
                error
                    .into_response()
                    .headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("application/json"),
                "{status}"
            );
        }
        assert_eq!(SESSION_STARTING, "session_starting");
        assert_eq!(SESSION_IDLE, "session_idle");
        assert_eq!(SESSION_RUNNING, "session_running");
        assert_eq!(SESSION_STOPPED, "session_stopped");
        assert_eq!(SESSION_ERROR, "session_error");
        assert_eq!(SESSION_DELETING, "session_deleting");
    }
}
