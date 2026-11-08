//! `POST /api/sessions/:id/merge`: land a session's branch on a target (HS-054, HS-055 and HS-056, spec item
//! 12's rules 1–6).
//!
//! The route is a `POST`, so every mutating guard applies and the manifest records it [`Mutating`] — and it is
//! the first route in this build whose work **writes to the project repository**. It is also the one session
//! route whose git work is not the backend's: item 12's sentence is "all git work for review and merge runs on
//! the host in the project repository; containers never merge", so the **merge** asks no backend anything. The
//! one exception reads `delete_session` (rule 5): that hands the row to the delete flow, which does ask the
//! backend for the session's own teardown, and which is the reason this handler cannot claim to be
//! backend-blind.
//!
//! Four scopes, in the order the handler takes them:
//!
//! 1. **the body** — taken as a `Result<Bytes, BytesRejection>` for the reason the create's is, and parsed by
//!    hand so `unknown_field` and `bad_strategy` are decisions rather than serde's error text;
//! 2. **the row** — one lock, no `.await` inside it; an unknown id is the envelope's `404 not_found`. Rule 1's
//!    **first guard** is the row's own fact: a `running` turn is `409 session_running`, and every other state is
//!    merged (ADR-0065 D2). A row with no `base_commit` never reaches git at all, because a session with no
//!    worktree has no branch to merge and answers `409 no_worktree` with **this route's** sentence (the code is
//!    the changes view's, the sentence describes the request — ADR-0063 D1);
//! 3. **rule 1's second guard** — the session's own worktree, read with the changes view's definition of work
//!    (`changes::dirty`, after `changes::basis`' four refusals). Uncommitted work is `409 dirty_worktree`, and
//!    the answer is a *pre*check on purpose: unlike `409 target_dirty`, whose checkout is the operator's and may
//!    hold work git carries across, this dirtiness is the request's own subject (ADR-0065 D3);
//! 4. **the merge** — [`crate::merge::merge`] on a blocking task (the whole operation is synchronous git, and a
//!    request path must not park a runtime worker on it), then the three keys, or one of the refusals. Both
//!    guards sit **before** this task, so a refused merge cannot have made `<data>/merge` at all.
//!
//! The worktree is built from the naming rule ([`Worktree::of`]) rather than from the row's recorded path, for
//! the reason `http::changes` gives: the rule derives the branch, the repository and the directory from the same
//! three facts, and the merge's basis refuses a path git does not register rather than trusting either spelling.
//!
//! The target's **first** fact — that it is a branch this machine will merge into — is asked before the row is
//! read, so the same bad request is answered the same way whatever the session's state is; the module asks it
//! again as its own precondition (`merge::listed`).
//!
//! **A merge changes nothing about the row** — it is neither a state transition nor an event, so the session
//! stays whatever it was and the branch is merged — **except** for `delete_session: true`, which runs the delete
//! flow's request half after the landing (rule 5). That row is `deleting` before this handler answers, so a
//! client cannot prompt a session whose work has just landed, and the removal itself runs off this task: a
//! `docker rm` is not a merge's business. The answer is the three keys either way (ADR-0065 D7). `GET …/changes`
//! is **unchanged** by a merge: `ahead` counts the branch's commits and the branch did not move, `dirty` is the
//! working tree's, and `behind`/`mergeable` were already what they are — a fast-forward requires the target to be
//! an ancestor of the branch, so `behind` was `0`; after a `merge` or a `squash` the target has gained a commit
//! the branch does not, and `behind` is the `1` that says so. Those fields were never *this route's* claim to
//! keep.

use std::sync::PoisonError;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::changes::{self, ChangesError};
use crate::http::app::AppState;
use crate::http::error::{ApiError, internal};
use crate::http::sessions;
use crate::merge::{MergeError, Request, Strategy, listed, merge};
use crate::sessions::Status;
use crate::worktree::Worktree;

/// The body's fields, exactly: any other key is `unknown_field`, the create's own rule.
///
/// All four are read: `message` names a commit for the two strategies that make one, and `delete_session` runs
/// the delete flow after a landing (HS-056 built it; it was the last field this build refused).
const FIELDS: [&str; 4] = ["target", "strategy", "message", "delete_session"];

/// `target` is not one of the configured `[merge] targets`: the config, not the repository, is what refused.
const UNLISTED_TARGET: &str = "`target` must be a branch listed in the `merge.targets` config key; this machine merges into no other";

/// `target` is configured and the repository has no such branch — the ordinary case of the default
/// `["main", "master"]` list on a machine that has one of them.
const MISSING_TARGET: &str = "`target` must be a branch this repository has: that name is configured, but the repository has no such \
     branch";

/// `strategy` is not one of the spec's three words at all — a malformed request rather than a missing feature.
const BAD_STRATEGY: &str = "`strategy` must be `ff-only`, `merge` or `squash`";

/// A merge of a session whose turn is running (HS-056, spec item 12's rule 1).
///
/// The same `session_running` code a prompt gets while a turn runs, and a sentence that describes **this**
/// request: nothing was merged, and the operator's move is to wait or to abort.
const RUNNING: &str = "a turn is running in this session, so nothing was merged; wait for it to end, or abort it, and merge again";

/// A merge of a session whose worktree holds uncommitted work (HS-056, rule 1).
///
/// "Only commits are merged", so the refusal names the two things that are true: nothing was merged, and the
/// next step is a commit. The offer to *make* that commit is HS-058's Changes-sheet affordance; ADR-0065 names
/// the prompt it sends, because a sentence the operator reads and a prompt the agent gets should not be two
/// guesses.
const DIRTY_WORKTREE: &str = "the session has uncommitted work, so nothing was merged; ask the agent to commit it, then merge again";

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
    let Parsed {
        request,
        delete_session,
    } = match parse_body(&body) {
        Ok(parsed) => parsed,
        Err(error) => return error.into_response(),
    };
    // The target's first fact is asked here and not later, because it is the **request's**: a body naming a
    // branch this machine will not merge into is malformed whatever the session's state is, so it must not be
    // answered differently for a session with no worktree than for one with (ADR-0012's order). `merge` asks
    // the same predicate again as its own precondition.
    if !listed(&request.target, &state.config.merge.targets) {
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
    // Rule 1's first guard, and the only state it refuses: a turn is in flight. `starting`, `stopped`, `error`
    // and `deleting` are merged — the table has one code per state, and a guard over any of them would be a rule
    // the spec does not name (ADR-0065 D2). A `!running` written as `!= Idle` would refuse the `error` row a
    // failed provision leaves behind, which is exactly the session an operator wants to merge.
    if session.status == Status::Running {
        return ApiError::not_in_state(Status::Running, RUNNING).into_response();
    }
    // The recorded base commit is the row's own witness that a worktree step completed, which is the same fact
    // `GET …/changes` refuses on and the same sentence: a merge has no branch to move without it.
    if session.base_commit.is_none() {
        return ApiError::no_worktree(NO_WORKTREE).into_response();
    }
    let worktree = Worktree::of(&state.config.projects_root, &session.project, &session.id);
    // Rule 1's second guard, and the one pre-check this route makes on purpose (ADR-0065 D3): the session's own
    // uncommitted work is the request's subject, so it is asked **before** git is asked to move anything. The
    // changes view's `basis` comes first so a worktree that is gone (a `rm -rf`, a pruned registration, a
    // deleted branch) answers this route's `409 no_worktree` instead of a `500` from `git status` in a directory
    // that is not there. `merge` asks the same four facts again as its own precondition — ADR-0063's shape for a
    // predicate a module will not take on its caller's word — so this route reads the same three paths twice.
    // The answer is the branch's tip as well as the dirtiness, and the tip is what rule 5's delete is allowed to
    // act on: a branch that has moved past this commit by the time the delete asks has work nothing landed, so the
    // keep-rule keeps it (ADR-0065 D6). Reading it here rather than after the merge is what makes the comparison
    // mean "since the request began" — a commit an agent makes during the landing is one this never saw.
    let unchecked = {
        let worktree = worktree.clone();
        tokio::task::spawn_blocking(move || -> Result<(String, bool), ChangesError> {
            let tip = changes::basis(&worktree)?;
            let dirty = changes::dirty(&worktree.path).map_err(ChangesError::Git)?;
            Ok((tip, dirty))
        })
        .await
    };
    let (tip, dirty) = match unchecked {
        Ok(Ok(answer)) => answer,
        Ok(Err(error)) => return changes_refusal(error).into_response(),
        Err(error) => return internal("merge", error).into_response(),
    };
    if dirty {
        return ApiError::dirty_worktree(DIRTY_WORKTREE).into_response();
    }
    let targets = state.config.merge.targets.clone();
    let merges = state.merges.clone();
    let data_root = state.config.data_root.clone();
    let merged = tokio::task::spawn_blocking(move || {
        merge(&worktree, &request, &targets, &merges, &data_root)
    })
    .await;
    match merged {
        Ok(Ok(merged)) => {
            if delete_session && session.status != Status::Deleting {
                // Rule 5, after a **successful** merge: the delete flow's request half, which moves the row to
                // `deleting` and hands the removal to a task. `force` is false — this route has no query, and a
                // request that forced a delete would be a second way to lose work (ADR-0065 D5). A **second**
                // teardown is not asked for either: a row that is already `deleting` is a removal in progress whose
                // keep-rule answer belongs to the request that started it (D2's price, stated there). A refusal
                // here is the delete's own and is **logged**: the 200 stands, because the work landed, and an
                // answer that said otherwise would tell the operator the merge failed.
                if let Err(error) = sessions::deletion(state, id, false, Some(&tip)).await {
                    tracing::warn!(
                        target: "holosmith::http::merge",
                        session = id,
                        status = error.status.as_u16(),
                        code = error.body.code,
                        message = %error.body.message,
                        "the work landed but the session was not deleted"
                    );
                }
            }
            (StatusCode::OK, Json(merged)).into_response()
        }
        Ok(Err(error)) => refusal_of(error).into_response(),
        // A task that panicked or was cancelled has said nothing about the repository: `internal` logs the
        // operation and the error once, and the client gets the one fixed sentence.
        Err(error) => internal("merge", error).into_response(),
    }
}

/// One accepted body: the merge's request and the one field the merge itself is not told about.
///
/// `delete_session` is the **route's** fact rather than the merge's, which is why it does not travel in
/// [`Request`]: `merge::merge` moves a ref and knows nothing about sessions, and a field it was handed and
/// never read would be a lie in its type (ADR-0065 D4).
#[derive(Debug)]
struct Parsed {
    request: Request,
    delete_session: bool,
}

/// Read the body as JSON, refuse an unknown field, then the fields this route cannot honour.
///
/// The order is the request's: shape, then unknown key, then `strategy`, then `message`'s type, then
/// `delete_session`'s type. A strategy this build does not know comes before the rest because the strategy is
/// the field that would change the *repository*.
///
/// `target` is not validated here beyond being a string: `merge.targets` is where a branch name is checked
/// (`config`'s `is_ref_name`), so membership in that list *is* the validation, and a second alphabet would be a
/// second answer to "what is a branch name". `message` has no length of its own — the create's `prompt` has
/// none either — and it reaches git as one argument of a `std::process::Command`, never through a shell.
fn parse_body(bytes: &[u8]) -> Result<Parsed, ApiError> {
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
    let strategy = match object.get("strategy") {
        // Absent means ff-only, which is item 12's default.
        None | Some(Value::Null) => Strategy::FastForward,
        Some(Value::String(named)) => {
            Strategy::parse(named).ok_or_else(|| ApiError::bad_strategy(BAD_STRATEGY))?
        }
        Some(_) => return Err(ApiError::bad_merge_body()),
    };
    let message = match object.get("message") {
        None | Some(Value::Null) => None,
        Some(Value::String(message)) if !message.trim().is_empty() => Some(message.clone()),
        // An empty message is `bad_body` and not "the default": a client that sent `"message": ""` asked for a
        // commit whose message is nothing, which is not the same request as one that named no message.
        Some(Value::String(_)) => return Err(ApiError::bad_merge_body()),
        Some(_) => return Err(ApiError::bad_merge_body()),
    };
    // Rule 5's field: `true` runs the delete flow after a landing, `false`/absent/null is a merge only, and
    // anything that is not a boolean is the body's *shape* — `bad_body`, like a numeric `strategy`.
    let delete_session = match object.get("delete_session") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(asked)) => *asked,
        Some(_) => return Err(ApiError::bad_merge_body()),
    };
    Ok(Parsed {
        request: Request {
            target,
            strategy,
            message,
        },
        delete_session,
    })
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
        // The one refusal that carries a payload: rule 3's paths, in the envelope's additive `conflicts` key,
        // because the message a `409` carries is a fixed sentence (ADR-0008) and a phone needs the list.
        MergeError::Conflict { paths } => ApiError::conflict(paths),
        MergeError::Temporary { path, detail } => internal(
            "merge",
            format!(
                "the temporary merge worktree {} could not be used: {detail}",
                path.display()
            ),
        ),
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

    fn parse(text: &str) -> Result<Parsed, ApiError> {
        parse_body(text.as_bytes())
    }

    /// The target of a body that is accepted, for the tests that are about `target` and not about the rest.
    fn target(text: &str) -> Option<String> {
        parse(text).ok().map(|parsed| parsed.request.target)
    }

    #[test]
    fn the_body_is_the_contract_s_three_spellings_of_a_target() {
        // Only a `target` is required; `strategy` absent is item 12's default, and an explicit `ff-only` is the
        // same request. `message` is now **read** by the two strategies that make a commit and ignored by the one
        // that does not, so a body that carries one is the contract's body and not an unknown field.
        assert_eq!(target(r#"{"target":"main"}"#).as_deref(), Some("main"));
        assert_eq!(
            target(r#"{"target":"main","strategy":"ff-only"}"#).as_deref(),
            Some("main")
        );
        assert_eq!(
            target(r#"{"target":"main","message":"a story","delete_session":false}"#).as_deref(),
            Some("main")
        );
    }

    #[test]
    fn all_three_of_the_spec_s_strategies_are_accepted_and_the_absent_one_is_ff_only() {
        // HS-054 refused two of these with `422 unsupported`; HS-055 builds them, so the request that names one
        // is the contract's request. What `parse_body` must get right is the **word to variant** mapping — the
        // rest of the story is what `merge::merge` does with it.
        assert_eq!(
            parse(r#"{"target":"main"}"#)
                .expect("a body")
                .request
                .strategy,
            Strategy::FastForward
        );
        for (text, expected) in [
            (
                r#"{"target":"main","strategy":"ff-only"}"#,
                Strategy::FastForward,
            ),
            (r#"{"target":"main","strategy":"merge"}"#, Strategy::Merge),
            (r#"{"target":"main","strategy":"squash"}"#, Strategy::Squash),
        ] {
            assert_eq!(
                parse(text).expect(text).request.strategy,
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn a_message_is_passed_through_and_an_empty_one_is_not_the_default() {
        // A message is the operator's words and reaches git verbatim. `""` is refused rather than read as "the
        // default": a client that sent it asked for a commit whose message is nothing, and answering that with
        // `Merge branch 'agent/x'` would be naming the commit something nobody asked for.
        assert_eq!(
            parse(r#"{"target":"main","strategy":"merge","message":"a story"}"#)
                .expect("a body")
                .request
                .message
                .as_deref(),
            Some("a story")
        );
        assert_eq!(
            parse(r#"{"target":"main","strategy":"squash","message":null}"#)
                .expect("a body")
                .request
                .message,
            None
        );
        for text in [
            r#"{"target":"main","message":""}"#,
            r#"{"target":"main","message":3}"#,
        ] {
            let error = parse(text).expect_err(text);
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
            assert_eq!(error.body.code, BAD_BODY, "{text}");
        }
    }

    #[test]
    fn delete_session_is_read_as_the_route_s_own_field_and_a_non_bool_is_bad_body() {
        // Rule 5's field, and the last one this build used to refuse: it is a `bool` outside `Request`, because
        // the merge moves a ref and knows nothing about sessions (ADR-0065 D4). `false` and a missing field are
        // the same request; a string is the body's *shape* and not a value outside a vocabulary, so `bad_body`.
        for (text, expected) in [
            (r#"{"target":"main"}"#, false),
            (r#"{"target":"main","delete_session":false}"#, false),
            (r#"{"target":"main","delete_session":null}"#, false),
            (r#"{"target":"main","delete_session":true}"#, true),
        ] {
            assert_eq!(parse(text).expect(text).delete_session, expected, "{text}");
        }
        for text in [
            r#"{"target":"main","delete_session":"yes"}"#,
            r#"{"target":"main","delete_session":1}"#,
        ] {
            let error = parse(text).expect_err(text);
            assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
            assert_eq!(error.body.code, BAD_BODY, "{text}");
        }
    }

    #[test]
    fn a_strategy_outside_the_spec_s_three_words_is_a_malformed_request() {
        // A wrong *value* is `bad_strategy`, and it is now the only strategy refusal there is: `rebase` is not one
        // of item 12's words, and the three that are all run.
        let error = parse(r#"{"target":"main","strategy":"rebase"}"#).expect_err("a fourth word");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.body.code, BAD_STRATEGY_CODE);
        let error = parse(r#"{"target":"main","strategy":3}"#).expect_err("a number");
        assert_eq!(error.body.code, BAD_BODY);
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
        let request =
            parse(r#"{"target":"main","strategy":null,"delete_session":null}"#).expect("a body");
        assert_eq!(request.request.target, "main");
        assert_eq!(request.request.strategy, Strategy::FastForward);
        assert!(!request.delete_session);
    }
}
