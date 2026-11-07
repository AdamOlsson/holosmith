//! `GET /api/sessions/:id/changes`: what a session changed and whether it can merge (HS-052, spec item 12).
//!
//! The route is a `GET`, so every guard applies exactly as it does to its neighbours and the manifest records
//! it `ReadOnly` — nothing here writes, on any surface. It is also the one session route whose work is **host
//! git** rather than the database or the backend: item 12's sentence is "all git work for review and merge runs
//! on the host in the project repository; containers never merge", so no container is asked anything and the
//! answer does not depend on which backend this run uses.
//!
//! Three scopes, in the order the handler takes them:
//!
//! 1. **the row** — one lock, no `.await` inside it, the same scope `sessions::one` takes. An unknown id is the
//!    envelope's `404 not_found`, and a row with no `base_commit` (a stub session, or a create that failed
//!    before its worktree step) never reaches git at all: there is nothing to review and the answer is
//!    `409 no_worktree`;
//! 2. **the git reads** — [`Changes::read`] on a blocking task, for the reason the provisioner and the project
//!    refusals use one: a request path must not park a runtime worker on a subprocess, and this is five of them;
//! 3. **the answer** — the eleven fields as JSON, or one of the four `409` sentences, or the `500` every
//!    unplanned failure gets with git's words in the log (ADR-0008).
//!
//! The worktree is built from the naming rule ([`Worktree::of`]) rather than from the row's recorded path for
//! the reason `DockerBackend` does the same: the rule derives the branch and the repository from the same three
//! facts, and [`Changes::read`] answers `409` for a path that git does not register rather than trusting either
//! spelling of it.

use std::sync::PoisonError;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::changes::{Changes, ChangesError};
use crate::http::app::AppState;
use crate::http::error::{ApiError, internal};
use crate::worktree::Worktree;

/// The sentence a row with no worktree earns: a stub session, or a create that stopped before that step.
const NO_WORKTREE: &str = "this session has no worktree, so there is nothing to review";

/// The sentence a worktree directory that is not there earns — and the next step, which is a resume.
///
/// A resume re-provisions the session, and `Worktree::ensure` adopts the branch that already holds the
/// commits, so the work is not lost by asking again: the directory comes back with every commit on it.
const WORKTREE_GONE: &str = "the session's worktree is gone, so there is nothing to review; resume the session to make it again \
     from the branch";

/// The sentence a directory that git no longer registers earns — a `git worktree prune` ran under it.
const NOT_REGISTERED: &str = "git no longer registers the session's worktree, so there is nothing to review; resume the session to \
     make it again from the branch";

/// The sentence a branch that is not in the repository earns.
const NO_BRANCH: &str = "the session's branch is gone, so there is nothing to compare";

/// The sentence a project repository that is not there earns.
const NO_REPOSITORY: &str = "the project's repository is gone, so there is nothing to compare against; restore the project's \
     checkout under the projects root";

/// Answer one session's changes view: 200 and the summary, `404` for an id the table does not hold, `409
/// no_worktree` when there is nothing to read, or ADR-0008's `500`.
pub(crate) async fn respond(state: &AppState, id: &str) -> Response {
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
    // The recorded base commit is the row's own witness that a worktree step completed — it and the path are
    // written by one statement (`set_worktree`) — so a row without one is answered before git is asked
    // anything. That is also what makes a stub session's answer a `409` and not a failed subprocess.
    let Some(base_commit) = session.base_commit.clone() else {
        return ApiError::no_worktree(NO_WORKTREE).into_response();
    };
    let worktree = Worktree::of(&state.config.projects_root, &session.project, &session.id);
    let targets = state.config.merge.targets.clone();
    let read =
        tokio::task::spawn_blocking(move || Changes::read(&worktree, &base_commit, &targets)).await;
    match read {
        Ok(Ok(changes)) => (StatusCode::OK, Json(changes)).into_response(),
        Ok(Err(error)) => refusal(error).into_response(),
        // A task that panicked or was cancelled has said nothing about the session: `internal` logs the
        // operation and the error once, and the client gets the one fixed sentence.
        Err(error) => internal("changes", error).into_response(),
    }
}

/// The envelope a [`ChangesError`] is answered with.
///
/// Four facts, one code, four sentences — and the one arm that is not a `409`: a repository git cannot read
/// has not been checked at all, which is the split `refusal_error` makes for a create's project refusals. The
/// path a sentence would have named goes to the log instead, because the envelope's messages are fixed
/// sentences (ADR-0008) and a path on a phone is noise beside the next step.
fn refusal(error: ChangesError) -> ApiError {
    match error {
        ChangesError::NoRepository { .. } => ApiError::no_worktree(NO_REPOSITORY),
        ChangesError::Missing { .. } => ApiError::no_worktree(WORKTREE_GONE),
        ChangesError::Unregistered { .. } => ApiError::no_worktree(NOT_REGISTERED),
        ChangesError::NoBranch { .. } => ApiError::no_worktree(NO_BRANCH),
        ChangesError::Git(error) => {
            // The sentence carries the command, the repository and git's own words — all of which belong in
            // the log rather than in an answer.
            tracing::warn!(
                target: "holosmith::http::changes",
                %error,
                "the changes view could not read the repository"
            );
            internal("changes", error)
        }
    }
}
