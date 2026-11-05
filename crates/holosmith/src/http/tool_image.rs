//! `GET /api/sessions/:id/tool-image/:tool_id/:index`: one image a tool returned (HS-035).
//!
//! An image in a tool result cannot go in an event payload — the log is replayed to a phone on every reconnect
//! and folded by compaction (ADR-0032, ADR-0041) — so it is a file beside the tool's text
//! (`crate::tool::Store::store_images`) and this is the one way to read it. `tool.end`'s `images` names it by
//! `[tool_id, index]`, and the **index is the block's position in `result.content`**: it names the block the
//! image came from, so two images of one result are told apart by where they were.
//!
//! It is a `GET`, so every guard applies exactly as it does to the other read-only routes and the manifest
//! records `ReadOnly` (ADR-0010, ADR-0017).
//!
//! **The content type is the store's, never the request's.** The file's extension decides it, and the request
//! cannot name an extension: a client asks for the pair the frame gave it, so a type cannot be talked into
//! something else by a URL. An image type this build cannot name is served as `application/octet-stream` — the
//! bytes are honest, and the frame's own `mime` is what a client shows.
//!
//! **A traversal is impossible by construction**, the same rule `tool-output` keeps: the tool id is checked
//! against an allow-list ([`crate::tool::valid_id`]) before anything is joined, and the index is a number, so
//! `..`, `/` and `.` are not characters either of them has. A name that fails either rule cannot be a file any
//! writer here created, so the answer is the same `404` a missing file gets — one question, one answer.

use std::sync::PoisonError;

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::http::app::AppState;
use crate::http::error::{ApiError, NOT_FOUND, internal};
use crate::tool::{Store, valid_id};

/// What a request naming no image answers.
///
/// One sentence for the four ways it can happen — a session the table does not hold, an id that cannot be a
/// filename, an index that is not a number, an image that is not there — because they are one fact to a
/// caller: there is nothing to fetch.
const NO_IMAGE: &str = "there is no tool image with that id for this session";

/// The envelope answer for "there is nothing to fetch": `404`, the spec's one `not_found` code, and the
/// sentence above.
fn no_image() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, NOT_FOUND, NO_IMAGE)
}

/// Answer one stored image: 200, its own bytes and its own type, `404 not_found` otherwise.
pub(crate) async fn respond(
    state: &AppState,
    session_id: &str,
    tool_id: &str,
    index: &str,
) -> Response {
    // The row first: an id the table does not hold is not a directory to look in, and a delete's leftovers
    // (HS-043's keep-rules) must not be reachable through a session nobody can see.
    let known = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        database.sessions().get(session_id)
    };
    match known {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::unknown_session().into_response(),
        Err(error) => return internal("get", error).into_response(),
    }
    if !valid_id(tool_id) {
        return no_image().into_response();
    }
    // A block's index is a `usize` in the frame and a decimal in a URL. Anything else is not a name this
    // store could have written, so it is the same `404` rather than a second reason to learn.
    let Ok(index) = index.parse::<usize>() else {
        return no_image().into_response();
    };
    let store = Store::new(&state.config.data_root);
    match store.read_image(session_id, tool_id, index) {
        Some((bytes, mime)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, header::HeaderValue::from_static(mime))],
            bytes,
        )
            .into_response(),
        None => no_image().into_response(),
    }
}
