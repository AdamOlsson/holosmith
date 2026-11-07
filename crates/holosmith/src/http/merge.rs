//! `POST /api/sessions/:id/merge`: land a session's branch on a target (HS-054, spec item 12's rules 2 and 4).
//!
//! The route is a `POST`, so every mutating guard applies and the manifest records it [`Mutating`] — and it is
//! the first route in this build whose work **writes to the project repository**. It is also the one session
//! route that is neither the database's nor the backend's: item 12's sentence is "all git work for review and
//! merge runs on the host in the project repository; containers never merge", so no container is asked anything
//! and this handler knows no backend.
//!
//! Three scopes, in the order the handler takes them:
//!
//! 1. **the body** — taken as a `Result<Bytes, BytesRejection>` for the reason the create's is, and parsed by
//!    hand so `unknown_field` and the two `unsupported` sentences are decisions rather than serde's error text;
//! 2. **the row** — one lock, no `.await` inside it; an unknown id is the envelope's `404 not_found`, and a row
//!    with no `base_commit` never reaches git at all, because a session with no worktree has no branch to merge
//!    and answers `409 no_worktree` with **this route's** sentence (the code is the changes view's, the sentence
//!    describes the request — ADR-0063 D1);
//! 3. **the merge** — [`crate::merge::merge`] on a blocking task (the whole operation is synchronous git, and a
//!    request path must not park a runtime worker on it), then the three keys, or one of the refusals.
//!
//! The worktree is built from the naming rule ([`Worktree::of`]) rather than from the row's recorded path, for
//! the reason `http::changes` gives: the rule derives the branch, the repository and the directory from the same
//! three facts, and the merge's basis refuses a path git does not register rather than trusting either spelling.
//!
//! The target's **first** fact — that it is a branch this machine will merge into — is asked before the row is
//! read, so the same bad request is answered the same way whatever the session's state is; the module asks it
//! again as its own precondition (`merge::listed`).
//!
//! **Nothing about the row changes.** A merge is not a state transition and not an event: the session stays
//! whatever it was and the branch is merged. `GET …/changes` is therefore **unchanged** by a merge — `ahead`
//! counts the branch's commits and the branch did not move, and `behind` was already zero because a fast-forward
//! requires the target to be an ancestor of the branch.

use std::sync::PoisonError;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::changes::ChangesError;
use crate::http::app::AppState;
use crate::http::error::{ApiError, internal};
use crate::merge::{FF_ONLY, MergeError, listed, merge};
use crate::worktree::Worktree;

/// The body's fields, exactly: any other key is `unknown_field`, the create's own rule.
///
/// `message` is named here and read by nothing. Spec item 12's body is `{target, strategy, message?,
/// delete_session?}`, and a `merge` or `squash` strategy is what a message would name — a fast-forward creates
/// no commit, so there is nothing for it to say. It is admitted rather than refused because a client that sends
/// the contract's body must not be told it sent a field this endpoint does not accept; HS-055, which builds the
/// two strategies that make a merge commit, is what will read it.
const FIELDS: [&str; 4] = ["target", "strategy", "message", "delete_session"];

/// `target` is not one of the configured `[merge] targets`: the config, not the repository, is what refused.
const UNLISTED_TARGET: &str = "`target` must be a branch listed in the `merge.targets` config key; this machine merges into no other";

/// `target` is configured and the repository has no such branch — the ordinary case of the default
/// `["main", "master"]` list on a machine that has one of them.
const MISSING_TARGET: &str = "`target` must be a branch this repository has: that name is configured, but the repository has no such \
     branch";

/// `strategy` named one of spec item 12's two strategies this build does not have yet.
///
/// The sentence says what *did* not happen, because the request asked for something that would have changed the
/// repository: an endpoint that silently fast-forwarded what was asked to squash would be lying about the work.
const UNBUILT_STRATEGY: &str = "`strategy` must be `ff-only` in this build: the `merge` and `squash` strategies are not built yet, so \
     nothing was merged";

/// `strategy` is not one of the spec's three words at all — a malformed request rather than a missing feature.
const BAD_STRATEGY: &str = "`strategy` must be `ff-only`, `merge` or `squash`";

/// `delete_session` was true and the delete flow is HS-056's.
///
/// The refusal comes *before* the merge, so the sentence has to say that nothing was merged either: a client
/// that read "the session was not deleted" might reasonably think its work had landed.
const UNBUILT_DELETE: &str = "`delete_session` is not built yet, so nothing was merged and the session was not deleted; merge with \
     `delete_session` absent, then delete the session";

/// The four `no_worktree` facts, in this route's words.
///
/// One **code** is shared with `GET …/changes` — the client's move is the same ("there is nothing here to act
/// on") — and the sentences are not, because a sentence must describe the request it answers and the summary's
/// end in "there is nothing to review" (ADR-0008, and the reason `ApiError::no_worktree` takes its message from
/// the caller at all). The next-step halves are the same and stay: a resume re-provisions the session and
/// `Worktree::ensure` adopts the branch that already holds the commits.
const NO_WORKTREE: &str = "this session has no worktree, so there is no branch to merge";

const WORKTREE_GONE: &str = "the session's worktree is gone, so there is nothing to merge; resume the session to make it again from \
     the branch";

const NOT_REGISTERED: &str = "git no longer registers the session's worktree, so there is nothing to merge; resume the session to make \
     it again from the branch";

const NO_BRANCH: &str = "the session's branch is gone, so there is nothing to merge";

const NO_REPOSITORY: &str = "the project's repository is gone, so there is nothing to merge into; restore the project's checkout \
     under the projects root";

/// A merge that could not fast-forward: the target has commits the branch does not.
const NOT_FAST_FORWARD: &str = "the target has commits the session's branch does not, so it cannot be fast-forwarded and nothing was \
     merged; merge the target into the session's branch and try again";

/// A merge whose target checkout holds local work the fast-forward would overwrite.
const TARGET_DIRTY: &str = "the checkout that holds the target has local changes the fast-forward would overwrite, so nothing was \
     merged; commit or stash them and merge again";

/// A merge whose target is checked out somewhere that is not the project's own checkout.
const TARGET_CHECKED_OUT: &str = "the target is checked out in a worktree other than the project's own checkout, so nothing was merged; \
     check out another branch in that worktree and merge again";

/// Answer `POST /api/sessions/{id}/merge`: 200 and `{merged_commit, target, strategy}`, or an envelope.
pub(crate) async fn respond(
    state: &AppState,
    id: &str,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        // Every rejection — a body hyper could not buffer, or one over axum's 2 MiB default — is this
        // request's `bad_body`, for the reason the create maps its own.
        Err(_) => return ApiError::bad_merge_body().into_response(),
    };
    let target = match parse_body(&body) {
        Ok(target) => target,
        Err(error) => return error.into_response(),
    };
    // The target's first fact is asked here and not later, because it is the **request's**: a body naming a
    // branch this machine will not merge into is malformed whatever the session's state is, so it must not be
    // answered differently for a session with no worktree than for one with (ADR-0012's order). `merge` asks
    // the same predicate again as its own precondition.
    if !listed(&target, &state.config.merge.targets) {
        return ApiError::bad_target(UNLISTED_TARGET).into_response();
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
    // `GET …/changes` refuses on and the same sentence: a merge has no branch to move without it.
    if session.base_commit.is_none() {
        return ApiError::no_worktree(NO_WORKTREE).into_response();
    }
    let worktree = Worktree::of(&state.config.projects_root, &session.project, &session.id);
    let targets = state.config.merge.targets.clone();
    let merges = state.merges.clone();
    let merged =
        tokio::task::spawn_blocking(move || merge(&worktree, &target, &targets, &merges)).await;
    match merged {
        Ok(Ok(merged)) => (StatusCode::OK, Json(merged)).into_response(),
        Ok(Err(error)) => refusal_of(error).into_response(),
        // A task that panicked or was cancelled has said nothing about the repository: `internal` logs the
        // operation and the error once, and the client gets the one fixed sentence.
        Err(error) => internal("merge", error).into_response(),
    }
}

/// Read the body as JSON, refuse an unknown field, then the two fields this build cannot honour.
///
/// The order is the request's: shape, then unknown key, then `strategy`, then `delete_session`. A strategy this
/// build does not have comes before a delete it does not have because the strategy is the field that would
/// change the *repository*; a request that mentions both has already asked for something that cannot run.
///
/// `target` is not validated here beyond being a string: `merge.targets` is where a branch name is checked
/// (`config`'s `is_ref_name`), so membership in that list *is* the validation, and a second alphabet would be a
/// second answer to "what is a branch name".
fn parse_body(bytes: &[u8]) -> Result<String, ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ApiError::bad_merge_body())?;
    let object = value.as_object().ok_or_else(ApiError::bad_merge_body)?;
    if object.keys().any(|field| !FIELDS.contains(&field.as_str())) {
        return Err(ApiError::unknown_field());
    }
    let target = object
        .get("target")
        .and_then(Value::as_str)
        .ok_or_else(ApiError::bad_merge_body)?
        .to_string();
    match object.get("strategy") {
        // Absent means ff-only, which is item 12's default and the only strategy this build has.
        None | Some(Value::Null) => {}
        Some(Value::String(named)) if named == FF_ONLY => {}
        Some(Value::String(named)) if named == "merge" || named == "squash" => {
            return Err(ApiError::unsupported(UNBUILT_STRATEGY));
        }
        Some(_) => return Err(ApiError::bad_strategy(BAD_STRATEGY)),
    }
    match object.get("delete_session") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => return Err(ApiError::unsupported(UNBUILT_DELETE)),
        Some(_) => return Err(ApiError::bad_merge_body()),
    }
    Ok(target)
}

/// The envelope a [`MergeError`] is answered with.
///
/// Every refusal is logged with [`MergeError::sentence`], which is where the target, the configured list and the
/// checkout's path belong: the envelope's messages are fixed sentences (ADR-0008) and a path on a phone is noise
/// beside the next step, while an operator reading the log wants exactly those facts.
fn refusal_of(error: MergeError) -> ApiError {
    tracing::warn!(
        target: "holosmith::http::merge",
        sentence = error.sentence(),
        "a merge was refused"
    );
    match error {
        MergeError::Changes(error) => changes_refusal(error),
        MergeError::Unlisted { .. } => ApiError::bad_target(UNLISTED_TARGET),
        MergeError::NoTarget { .. } => ApiError::bad_target(MISSING_TARGET),
        MergeError::NotFastForward { .. } => ApiError::not_fast_forward(NOT_FAST_FORWARD),
        MergeError::TargetDirty { .. } => ApiError::target_dirty(TARGET_DIRTY),
        MergeError::CheckedOutElsewhere { .. } => ApiError::target_checked_out(TARGET_CHECKED_OUT),
    }
}

/// The envelope a [`ChangesError`] is answered with on **this** route: the same code, this request's sentences.
///
/// The state facts are one code with four sentences (ADR-0063 D1), and the one arm that is not a `409` is a
/// repository git cannot read — a machine problem, answered by `internal` with the operation *and* git's words,
/// which is the split `http::changes` makes for its own route.
fn changes_refusal(error: ChangesError) -> ApiError {
    match error {
        ChangesError::NoRepository { .. } => ApiError::no_worktree(NO_REPOSITORY),
        ChangesError::Missing { .. } => ApiError::no_worktree(WORKTREE_GONE),
        ChangesError::Unregistered { .. } => ApiError::no_worktree(NOT_REGISTERED),
        ChangesError::NoBranch { .. } => ApiError::no_worktree(NO_BRANCH),
        ChangesError::Git(error) => internal("merge", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::error::{
        BAD_BODY, BAD_STRATEGY as BAD_STRATEGY_CODE, BAD_TARGET, UNKNOWN_FIELD,
    };

    fn parse(text: &str) -> Result<String, ApiError> {
        parse_body(text.as_bytes())
    }

    #[test]
    fn the_body_is_the_contract_s_three_spellings_of_a_target() {
        // Only a `target` is required; `strategy` absent is item 12's default, and an explicit `ff-only` is the
        // same request. `message` is admitted and unread (a fast-forward has no commit to name), so a body that
        // carries one is the contract's body and not an unknown field.
        assert_eq!(parse(r#"{"target":"main"}"#).ok().as_deref(), Some("main"));
        assert_eq!(
            parse(r#"{"target":"main","strategy":"ff-only"}"#)
                .ok()
                .as_deref(),
            Some("main")
        );
        assert_eq!(
            parse(r#"{"target":"main","message":"a story","delete_session":false}"#)
                .ok()
                .as_deref(),
            Some("main")
        );
    }

    #[test]
    fn the_two_fields_this_build_cannot_honour_are_refused_before_anything_runs() {
        // The decision ADR-0063 records: refusing loudly beats ignoring a request that says "delete the
        // session". Both are `422` and both are `unsupported`, with a sentence each, because their next steps
        // differ — and neither is `bad_body`, because the body is the contract's.
        for (text, code) in [
            (r#"{"target":"main","strategy":"squash"}"#, "unsupported"),
            (r#"{"target":"main","strategy":"merge"}"#, "unsupported"),
            (r#"{"target":"main","delete_session":true}"#, "unsupported"),
        ] {
            let error = parse(text).expect_err(text);
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
            assert_eq!(error.body.code, code, "{text}");
        }
    }

    #[test]
    fn a_strategy_outside_the_spec_s_three_words_is_a_malformed_request() {
        // A missing *feature* and a wrong *value* are two different refusals: `rebase` is not one of item 12's
        // words, so it is `bad_strategy` rather than `unsupported`, which would promise that HS-055 adds it.
        let error = parse(r#"{"target":"main","strategy":"rebase"}"#).expect_err("a fourth word");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.body.code, BAD_STRATEGY_CODE);
    }

    #[test]
    fn a_body_that_is_not_the_contract_is_bad_body_and_a_fifth_key_is_unknown_field() {
        // The create's precedence test, for this route: shape first, then the key set. A client that sent
        // `{"target": "main", "force": true}` is owed `unknown_field` — a `200` for a request whose field was
        // ignored is the one answer that cannot be recovered from.
        for text in [
            "",
            "[]",
            "null",
            r#"{"strategy":"ff-only"}"#,
            r#"{"target":3}"#,
        ] {
            let error = parse(text).expect_err(text);
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
            assert_eq!(error.body.code, BAD_BODY, "{text}");
        }
        let error = parse(r#"{"target":"main","force":true}"#).expect_err("a fifth key");
        assert_eq!(error.body.code, UNKNOWN_FIELD);
        let error =
            parse(r#"{"target":"main","delete_session":"yes"}"#).expect_err("a string for a bool");
        assert_eq!(error.body.code, BAD_BODY);
        assert_ne!(BAD_TARGET, BAD_BODY);
    }

    #[test]
    fn a_null_is_the_same_answer_as_an_absent_field() {
        // `{"strategy":null}` is how a client that renders its form sparsely spells "not chosen", and the
        // contract's optional fields are optional. A null for `delete_session` is likewise "no".
        assert_eq!(
            parse(r#"{"target":"main","strategy":null,"delete_session":null}"#)
                .ok()
                .as_deref(),
            Some("main")
        );
    }
}
