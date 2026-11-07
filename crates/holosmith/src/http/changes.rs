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
use axum::extract::RawQuery;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::changes::{Changes, ChangesError, DiffError, diff};
use crate::http::app::AppState;
use crate::http::error::{ApiError, NOT_FOUND, internal};
use crate::worktree::Worktree;

/// The sentence a row with no worktree earns: a stub session, or a create that stopped before that step.
///
/// `pub(crate)` since HS-054: a merge has the same fact (there is no branch to merge either) and answers it with
/// the same sentence, because a client that read this one on `GET …/changes` should not have to learn a second
/// spelling of it on `POST …/merge`.
pub(crate) const NO_WORKTREE: &str = "this session has no worktree, so there is nothing to review";

/// The sentence a `path` this session did not change earns.
///
/// A fixed sentence, like every other envelope message (ADR-0008), so it cannot echo a path back — the path a
/// client sent goes to the log. It names the next step, which is where a client gets a good value: `files[]`
/// on the summary, which is the same list this route validates against.
const NOT_CHANGED: &str = "this session's changes do not include that file, so there is no diff of it; list \
                            the session's changes and pick a path from files[]";

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

/// Answer one session's unified diff: 200 and the patch as text, or an envelope (HS-053).
///
/// The scope is the summary's, in the same order and for the same reasons: one lock for the row (never held
/// across an `.await`), the row's `base_commit` or the same `409 no_worktree`, the worktree from the naming
/// rule, and the git read on a blocking task. Only the read is new — [`diff`] — and only one answer is: a
/// `path` this session did not change is a `404`, the resource a client asked for rather than a state of the
/// session.
///
/// The body is `text/plain`, because a patch is what a browser can show: no JSON envelope wraps it, and the
/// marker for a truncated one is a line of the body rather than a header the reader would never see. The
/// `charset` is claimed only when the bytes really are UTF-8 — git prints a file's own bytes, so a Latin-1
/// file's patch is not, and a charset that is a guess is worse than none.
pub(crate) async fn respond_diff(state: &AppState, id: &str, query: RawQuery) -> Response {
    let path = path_of(query.0.as_deref());
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
    let Some(base_commit) = session.base_commit.clone() else {
        return ApiError::no_worktree(NO_WORKTREE).into_response();
    };
    let worktree = Worktree::of(&state.config.projects_root, &session.project, &session.id);
    let read =
        tokio::task::spawn_blocking(move || diff(&worktree, &base_commit, path.as_deref())).await;
    match read {
        Ok(Ok(patch)) => {
            let content_type = if patch.utf8 {
                "text/plain; charset=utf-8"
            } else {
                "text/plain"
            };
            (
                StatusCode::OK,
                [(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static(content_type),
                )],
                patch.body,
            )
                .into_response()
        }
        Ok(Err(error)) => diff_refusal(error).into_response(),
        Err(error) => internal("changes_diff", error).into_response(),
    }
}

/// The envelope a [`DiffError`] is answered with: the summary's four refusals, and the one new fact.
fn diff_refusal(error: DiffError) -> ApiError {
    match error {
        DiffError::Changes(error) => refusal(error),
        // The path goes to the log rather than into the sentence, for the reason every envelope message is fixed
        // (ADR-0008): it is the client's own input, and it is what an operator needs when a client's encoding is
        // the thing that is wrong. `sentence()` is what puts it there, so the arm has one spelling.
        other => {
            tracing::debug!(
                target: "holosmith::http::changes",
                sentence = other.sentence(),
                "the diff was asked for a path this session did not change"
            );
            ApiError::new(StatusCode::NOT_FOUND, NOT_FOUND, NOT_CHANGED)
        }
    }
}

/// The `path` a request names, decoded, or `None` when it names none (HS-053).
///
/// No `path` is the whole branch, which is why absence and an empty value are different: `?path=` names the
/// empty name, which no file has, so it is a `404` rather than a request for everything. Every other parameter
/// is ignored rather than refused, the way the delete reads `?force` and history reads its query.
fn path_of(query: Option<&str>) -> Option<String> {
    let mut named: Option<String> = None;
    for pair in query.unwrap_or_default().split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name == "path" {
            named = Some(decode(value));
        }
    }
    named
}

/// Percent-decode a query value: `%20` is a space, `+` is a space, everything else is itself.
///
/// Written out rather than pulled in, the house rule `tool::decode_base64` follows: the input is a query string
/// and the alphabet is two hex digits. Decoding happens *before* the membership test, so a client that encodes a
/// space arrives with the name git printed — and an escape that is not two hex digits stays literal rather than
/// failing the request, because the value's only use is to be compared with a path git printed, and a literal
/// `%zz` is simply a name no file has. **Nothing here is a path**: the result is compared, never joined.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                match (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        out.push(high * 16 + low);
                        index += 3;
                    }
                    _ => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    // A decoded escape is a byte, and a multi-byte character arrives as several of them; a value that is not
    // UTF-8 after decoding is read the way `files[]` reads a path (lossily), so the two sides of the
    // comparison are spelled the same way.
    String::from_utf8_lossy(&out).into_owned()
}

/// One hex digit, or `None` when the byte is not one.
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The envelope a [`ChangesError`] is answered with.
///
/// Four facts, one code, four sentences — and the one arm that is not a `409`: a repository git cannot read
/// has not been checked at all, which is the split `refusal_error` makes for a create's project refusals. The
/// path a sentence would have named goes to the log instead, because the envelope's messages are fixed
/// sentences (ADR-0008) and a path on a phone is noise beside the next step.
///
/// `pub(crate)` since HS-054, whose merge inherits the whole family: it is one rule with one spelling rather
/// than a second copy that could drift the first time an arm is added.
pub(crate) fn refusal(error: ChangesError) -> ApiError {
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
