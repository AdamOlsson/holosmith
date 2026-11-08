//! `POST /api/sessions/:id/publish`: push a session's branch to a remote this machine may push to (HS-057,
//! spec item 12's publish paragraph).
//!
//! The route is a `POST`, so every mutating guard applies and the manifest records it [`Mutating`] — and it is
//! the first route in this build whose work reaches **out of the machine**. Like the merge it is a host
//! operation and asks no backend anything: item 12's sentence is "containers never hold a git credential", and
//! the push runs as the server's user with the server's own git ([`crate::publish`] says why that is the whole
//! of "the host's own credentials").
//!
//! Five scopes, in the order the handler takes them:
//!
//! 1. **the body** — the create's own shape, taken as a `Result<Bytes, BytesRejection>` so a rejection is this
//!    request's `bad_body` rather than axum's `text/plain`, and parsed by hand so `unknown_field` is a decision
//!    rather than serde's error text;
//! 2. **the machine's switch** — an empty `publish.remotes` is `403 publish_disabled`, and it is asked **before**
//!    the row: the endpoint is off for every id, so an unknown one is not this route's first answer (ADR-0066
//!    D4). Then `confirm` — `false` is a request that asked not to push (`422 confirm_required`), a missing or
//!    non-boolean one is the body's *shape* (`bad_body`, the merge's split);
//! 3. **the request's names** — `remote` must be listed in the config (`422 bad_remote`), `branch` must be a ref
//!    name (`bad_body`, the config's own alphabet), not protected (`422 protected_branch`) and not a configured
//!    `[merge] target` (`422 target_not_pushable`). All four are the request's facts and all four sit before the
//!    blocking task, so a refused publish touches nothing at all;
//! 4. **the row** — one lock, no `.await` inside it; an unknown id is the envelope's `404 not_found`. A row with
//!    no `base_commit` never reaches git: it has no worktree, so it has no branch to publish (`409 no_worktree`
//!    with **this route's** sentence);
//! 5. **the push** — [`crate::publish::publish`] on a blocking task (it is synchronous git, and a request path
//!    must not park a runtime worker on it), then the three keys or one of the refusals. The branch's four state
//!    refusals are the changes view's [`changes::basis`], answered as this route's `409 no_worktree`.
//!
//! **Nothing about the row changes.** A publish is neither a transition nor an event: the session stays whatever
//! it was — `idle`, `stopped`, even `error` — because pushing a branch is not work the session does. `GET
//! …/changes` is unchanged by one too: the branch did not move, so `ahead`, `behind`, `dirty` and `mergeable`
//! are the same answers as before.
//!
//! **The CI warning is not here.** `touches_ci` is the changes view's field (HS-052) and the warning before the
//! confirm step is HS-058's sheet; this route reads none of it, and the answer is exactly three keys (ADR-0066
//! D11). A publish whose answer carried a flag the route never computed would be a claim about a diff it did not
//! read.

use std::sync::PoisonError;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::changes::ChangesError;
use crate::config::is_ref_name;
use crate::http::app::AppState;
use crate::http::error::{ApiError, internal};
use crate::publish::{self, PublishError, Request, destination, listed, protected};
use crate::worktree::Worktree;

/// The body's fields, exactly: any other key is `unknown_field`, the create's own rule.
///
/// All three are read: `remote` names the remote, `branch` names the **destination** ref, and `confirm` is item
/// 12's opt-in — a publish is the one request in this build that changes something outside this machine, and the
/// spec asks the client to say so out loud.
const FIELDS: [&str; 3] = ["remote", "branch", "confirm"];

/// `remote` is not one of the configured `[publish] remotes`: the config, not the repository, is what refused.
///
/// Every refusal in this route says what did **not** happen, so a client that shows the sentence alone is not left
/// guessing whether a push went out (the gate asserts the phrase on each arm).
const UNLISTED_REMOTE: &str = "`remote` must be a remote named in the `publish.remotes` config key, so nothing was pushed; this machine \
     pushes to no other";

/// `remote` is configured and this project's repository has no such remote — the ordinary case of a name that is
/// per-machine and a URL that is per-repository.
const MISSING_REMOTE: &str = "`remote` must be a remote this project's repository has, so nothing was pushed: that name is configured, \
     but the repository has no such remote";

/// Item 12's protected names, as a refusal of the **destination**.
const PROTECTED_BRANCH: &str = "`branch` is a protected name, so nothing was pushed; this machine never pushes `main`, `master` or a \
     `release/*` branch";

/// Item 12's "pushing the merged target branch is not supported from the app; push main from the host".
const TARGET: &str =
    "`branch` is this machine's merge target, so nothing was pushed; push the target from the host";

/// `confirm: false`: the request asked **not** to push, so nothing happened and the answer says so.
///
/// A refusal rather than a `200` no-op: the request carried a decision, and answering it with the same body a
/// confirmed publish gets would tell the client its form was submitted when it was not.
const CONFIRM_REQUIRED: &str = "`confirm` must be `true` for a publish, so nothing was pushed";

/// The push the remote refused — a non-fast-forward, a stale lease, a server-side rule.
///
/// This build never force-pushes, so the next step is the operator's and the sentence names it. The reason git
/// gave is in the log, because the envelope's message is a fixed sentence (ADR-0008) and the reason is a word
/// from git's vocabulary, not a next step.
const REJECTED: &str = "the remote refused the push, so nothing was pushed; this build never force-pushes, so fetch the remote's \
     branch, integrate it and publish again";

/// The four `no_worktree` facts, in this route's words.
///
/// One **code** is shared with the changes view and the merge — the client's move is the same ("there is nothing
/// here to act on") — and the sentences are not, because a sentence must describe the request it answers
/// (ADR-0008, and the reason `ApiError::no_worktree` takes its message from the caller at all).
const NO_WORKTREE: &str = "this session has no worktree, so there is no branch to publish";

const WORKTREE_GONE: &str = "the session's worktree is gone, so there is nothing to publish; resume the session to make it again \
     from the branch";

const NOT_REGISTERED: &str = "git no longer registers the session's worktree, so there is nothing to publish; resume the session to \
     make it again from the branch";

const NO_BRANCH: &str = "the session's branch is gone, so there is nothing to publish";

const NO_REPOSITORY: &str = "the project's repository is gone, so there is nothing to publish from; restore the project's checkout \
     under the projects root";

/// Answer `POST /api/sessions/{id}/publish`: 200 and `{remote, branch, commit}`, or an envelope.
pub(crate) async fn respond(
    state: &AppState,
    id: &str,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        // Every rejection — a body hyper could not buffer, or one over axum's 2 MiB default — is this
        // request's `bad_body`, for the reason the create maps its own.
        Err(_) => return ApiError::bad_publish_body().into_response(),
    };
    let Parsed { request, confirm } = match parse_body(&body) {
        Ok(parsed) => parsed,
        Err(error) => return error.into_response(),
    };
    let remotes = &state.config.publish.remotes;
    // The machine's switch comes before any request fact: an endpoint the config turned off answers for every id,
    // so an unknown session is not the first thing it says. A `403` and not a `404` — the route exists (ADR-0066
    // D4).
    if remotes.is_empty() {
        return ApiError::publish_disabled().into_response();
    }
    if !confirm {
        return ApiError::confirm_required(CONFIRM_REQUIRED).into_response();
    }
    // The request's three names, in the order an operator would ask about them: the remote (is this machine
    // allowed to push there?), then the destination (may this build write that ref at all?), then the target (is
    // that the branch this machine lands on?). All three are answered whatever the session is, which is what
    // makes the same malformed body the same answer (ADR-0012's order).
    if !listed(&request.remote, remotes) {
        return ApiError::bad_remote(UNLISTED_REMOTE).into_response();
    }
    let branch = destination(id, &request);
    if protected(&branch) {
        return ApiError::protected_branch(PROTECTED_BRANCH).into_response();
    }
    if state
        .config
        .merge
        .targets
        .iter()
        .any(|target| target == &branch)
    {
        return ApiError::target_not_pushable(TARGET).into_response();
    }
    let session = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        database.sessions().get(id)
    };
    let session = match session {
        Ok(Some(session)) => session,
        Ok(None) => return ApiError::unknown_session().into_response(),
        Err(error) => return internal("get", error).into_response(),
    };
    // The recorded base commit is the row's own witness that a worktree step completed, which is the same fact
    // the changes view and the merge refuse on: a publish has no branch without it.
    if session.base_commit.is_none() {
        return ApiError::no_worktree(NO_WORKTREE).into_response();
    }
    let worktree = Worktree::of(&state.config.projects_root, &session.project, &session.id);
    let targets = state.config.merge.targets.clone();
    let remotes = remotes.clone();
    let pushed = tokio::task::spawn_blocking(move || {
        publish::publish(&worktree, &request, &remotes, &targets)
    })
    .await;
    match pushed {
        Ok(Ok(pushed)) => (StatusCode::OK, Json(pushed)).into_response(),
        Ok(Err(error)) => refusal_of(error).into_response(),
        // A task that panicked or was cancelled has said nothing about the remote: `internal` logs the operation
        // and the error once, and the client gets the one fixed sentence.
        Err(error) => internal("publish", error).into_response(),
    }
}

/// One accepted body: the publish's request and the one field the push itself is not told about.
///
/// `confirm` is the **route's** fact rather than the module's, which is why it does not travel in [`Request`]:
/// `publish::publish` pushes a ref and knows nothing about a client's form, and a field it was handed and never
/// read would be a lie in its type (ADR-0065 D4's shape, one route over).
#[derive(Debug)]
struct Parsed {
    request: Request,
    confirm: bool,
}

/// Read the body as JSON, refuse an unknown field, then the fields this route cannot honour.
///
/// The order is the request's shape: object → key set → `remote`'s type → `branch`'s type and alphabet →
/// `confirm`'s type. `remote`'s *membership* in the config is not checked here — that is a machine fact and the
/// handler asks it after the switch, so a body naming a remote nobody lists gets the sentence about the list
/// rather than about the body.
///
/// `confirm` is **required**: item 12's contract writes `confirm:true` in the request, and a publish is the one
/// request that reaches outside this machine. Absent is the body's shape (`bad_body`); `false` is a decision and
/// the handler's `confirm_required`.
fn parse_body(bytes: &[u8]) -> Result<Parsed, ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ApiError::bad_publish_body())?;
    let object = value.as_object().ok_or_else(ApiError::bad_publish_body)?;
    if object.keys().any(|field| !FIELDS.contains(&field.as_str())) {
        return Err(ApiError::unknown_field());
    }
    let remote = object
        .get("remote")
        .and_then(Value::as_str)
        .ok_or_else(ApiError::bad_publish_body)?
        .to_string();
    let branch = match object.get("branch") {
        None | Some(Value::Null) => None,
        // An empty destination is not "the default": the request named a ref whose name is nothing, which is not
        // the same request as one that named no branch. The merge's empty `message` is the same split.
        Some(Value::String(branch)) if !branch.is_empty() => Some(branch.clone()),
        Some(_) => return Err(ApiError::bad_publish_body()),
    };
    // The machine's one alphabet for a branch name (`config::is_ref_name`, which `merge.targets` is validated
    // with). A name git would refuse is the body's shape, not a state and not a machine problem: there is no
    // list to ask about a destination, so the alphabet *is* the validation.
    if let Some(branch) = &branch
        && !is_ref_name(branch)
    {
        return Err(ApiError::bad_publish_body());
    }
    let confirm = object
        .get("confirm")
        .and_then(Value::as_bool)
        .ok_or_else(ApiError::bad_publish_body)?;
    Ok(Parsed {
        request: Request { remote, branch },
        confirm,
    })
}

/// The envelope a [`PublishError`] is answered with.
///
/// Every refusal is logged with [`PublishError::sentence`], which is where the remote, the configured list and
/// git's reason word belong: the envelope's messages are fixed sentences (ADR-0008), and an operator reading the
/// log wants exactly those facts.
fn refusal_of(error: PublishError) -> ApiError {
    tracing::warn!(
        target: "holosmith::http::publish",
        sentence = error.sentence(),
        "a publish was refused"
    );
    match error {
        PublishError::Changes(error) => changes_refusal(error),
        PublishError::Unlisted { .. } => ApiError::bad_remote(UNLISTED_REMOTE),
        PublishError::NoRemote { .. } => ApiError::bad_remote(MISSING_REMOTE),
        PublishError::Protected { .. } => ApiError::protected_branch(PROTECTED_BRANCH),
        PublishError::Target { .. } => ApiError::target_not_pushable(TARGET),
        PublishError::Rejected { .. } => ApiError::push_rejected(REJECTED),
    }
}

/// The envelope a [`ChangesError`] is answered with on **this** route: the same code, this request's sentences.
///
/// The one arm that is not a `409` is a repository git cannot read — a machine problem, answered by `internal`
/// with the operation *and* git's words, which is the split `http::changes` and `http::merge` make for their own
/// routes.
fn changes_refusal(error: ChangesError) -> ApiError {
    match error {
        ChangesError::NoRepository { .. } => ApiError::no_worktree(NO_REPOSITORY),
        ChangesError::Missing { .. } => ApiError::no_worktree(WORKTREE_GONE),
        ChangesError::Unregistered { .. } => ApiError::no_worktree(NOT_REGISTERED),
        ChangesError::NoBranch { .. } => ApiError::no_worktree(NO_BRANCH),
        ChangesError::Git(error) => internal("publish", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::error::{
        BAD_BODY as BAD_BODY_CODE, BAD_REMOTE, CONFIRM_REQUIRED as CONFIRM_REQUIRED_CODE,
        PROTECTED_BRANCH as PROTECTED_BRANCH_CODE, TARGET_NOT_PUSHABLE, UNKNOWN_FIELD,
    };

    fn parse(text: &str) -> Result<Parsed, ApiError> {
        parse_body(text.as_bytes())
    }

    #[test]
    fn the_body_is_the_contract_s_two_shapes_of_a_publish() {
        // A publish must say `confirm: true` — item 12 writes it in the request, and this is the one request that
        // reaches outside the machine. `branch` is optional and a null is the same as absent (`strategy`'s and
        // `delete_session`'s rule on the merge).
        let parsed = parse(r#"{"remote":"origin","confirm":true}"#).expect("a body");
        assert_eq!(parsed.request.remote, "origin");
        assert_eq!(parsed.request.branch, None);
        assert!(parsed.confirm);
        let parsed =
            parse(r#"{"remote":"origin","branch":"pr/one","confirm":true}"#).expect("a body");
        assert_eq!(parsed.request.branch.as_deref(), Some("pr/one"));
        let parsed = parse(r#"{"remote":"origin","branch":null,"confirm":true}"#).expect("a body");
        assert_eq!(parsed.request.branch, None);
    }

    #[test]
    fn confirm_is_required_and_false_is_a_decision_not_a_shape_error() {
        // Absent, null or a non-boolean is the body's *shape* (`bad_body`); `false` is a request that asked not to
        // push, and the handler answers it `confirm_required`. The two are different codes because they are
        // different mistakes: one is a broken client, the other is a client that meant it.
        for text in [
            r#"{"remote":"origin"}"#,
            r#"{"remote":"origin","confirm":null}"#,
            r#"{"remote":"origin","confirm":"yes"}"#,
            r#"{"remote":"origin","confirm":1}"#,
        ] {
            let error = parse(text).expect_err(text);
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
            assert_eq!(error.body.code, BAD_BODY_CODE, "{text}");
        }
        assert!(
            !parse(r#"{"remote":"origin","confirm":false}"#)
                .expect("a body")
                .confirm
        );
    }

    #[test]
    fn a_body_that_is_not_the_contract_is_bad_body_and_a_fourth_key_is_unknown_field() {
        // The create's precedence test, for this route: shape first, then the key set. A client that sent
        // `{"remote":"origin","confirm":true,"force":true}` is owed `unknown_field` — a `200` for a request whose
        // field was ignored is the one answer that cannot be recovered from.
        for text in [
            "",
            "[]",
            "null",
            r#"{"confirm":true}"#,
            r#"{"remote":3,"confirm":true}"#,
        ] {
            let error = parse(text).expect_err(text);
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
            assert_eq!(error.body.code, BAD_BODY_CODE, "{text}");
        }
        let error =
            parse(r#"{"remote":"origin","confirm":true,"force":true}"#).expect_err("a fourth key");
        assert_eq!(error.body.code, UNKNOWN_FIELD);
        // The predicates the handler answers with are four different codes, and none of them is `bad_body`.
        assert_ne!(BAD_REMOTE, BAD_BODY_CODE);
        assert_ne!(PROTECTED_BRANCH_CODE, BAD_REMOTE);
        assert_ne!(TARGET_NOT_PUSHABLE, PROTECTED_BRANCH_CODE);
        assert_ne!(CONFIRM_REQUIRED_CODE, BAD_BODY_CODE);
    }

    #[test]
    fn every_refusal_says_that_nothing_was_pushed() {
        // The client shows the sentence alone, so a refusal that left "did anything go out?" open would be the one
        // question an operator cannot answer from a phone. This is the list the gate also walks at the wire, kept
        // here as one assertion so a *new* refusal cannot be added without the half (a gate can only assert the
        // arms it knows).
        for sentence in [
            UNLISTED_REMOTE,
            MISSING_REMOTE,
            PROTECTED_BRANCH,
            TARGET,
            CONFIRM_REQUIRED,
            REJECTED,
        ] {
            assert!(
                sentence.contains("nothing was pushed"),
                "{sentence} does not say what did not happen"
            );
        }
    }

    #[test]
    fn an_empty_or_impossible_destination_is_the_body_s_shape() {
        // An empty `branch` is not "the default" (which is the absent field), and a name git would refuse is not a
        // list question — there is no list, so the alphabet *is* the validation. `is_ref_name` is the config's own
        // (`merge.targets` is checked with it), so this route and the config cannot disagree about what a branch
        // name is.
        for text in [
            r#"{"remote":"origin","branch":"","confirm":true}"#,
            r#"{"remote":"origin","branch":"-x","confirm":true}"#,
            r#"{"remote":"origin","branch":"a..b","confirm":true}"#,
            r#"{"remote":"origin","branch":"a b","confirm":true}"#,
            r#"{"remote":"origin","branch":"release/x\n","confirm":true}"#,
            r#"{"remote":"origin","branch":3,"confirm":true}"#,
        ] {
            let error = parse(text).expect_err(text);
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
            assert_eq!(error.body.code, BAD_BODY_CODE, "{text}");
        }
        // ... and the two names item 12 cares about are *accepted* by the parser and refused by the handler's
        // predicates, which is why they are separate functions: the shape is the body's, the policy is the
        // machine's.
        assert!(
            parse(r#"{"remote":"origin","branch":"main","confirm":true}"#).is_ok(),
            "a protected name is a well-formed request"
        );
    }
}
