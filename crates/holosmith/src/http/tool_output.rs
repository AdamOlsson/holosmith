//! `GET /api/sessions/:id/tool-output/:tool_id`: one tool call's full output (HS-033).
//!
//! The transcript carries a preview of what a tool printed — a `cat` of a large file in an event payload would
//! be replayed to a phone on every reconnect and folded by compaction — so the rest is a file
//! (`crate::tool`) and this is the one way to read it. It is a `GET`, so every guard applies exactly as it
//! does to the other read-only routes and the manifest records `ReadOnly` (ADR-0010, ADR-0017).
//!
//! **A traversal is impossible by construction, not by cleaning up.** The tool id is checked against an
//! allow-list ([`crate::tool::valid_id`]: letters, digits, `-` and `_`) *before* anything is joined, so `..`,
//! `/` and `.` are not characters an id has. A name that fails the rule cannot be a file any writer here
//! created, so the answer is the same `404` a missing file gets — one question, one answer, and no second
//! reason an operator has to learn.
//!
//! `text/plain` rather than `application/octet-stream`: what is stored is the tool's own text (`crate::tool`
//! cuts on a character boundary, so a file is valid UTF-8 by construction), and a link that opens in a browser
//! should read as text.

use std::sync::PoisonError;

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::http::app::AppState;
use crate::http::error::{ApiError, NOT_FOUND, internal};
use crate::tool::{Store, valid_id};

/// What a request naming no output answers.
///
/// One sentence for the three ways it can happen — a session the table does not hold, an id that cannot be a
/// filename, a file that is not there — because they are one fact to a caller: there is nothing to fetch.
const NO_OUTPUT: &str = "there is no tool output with that id for this session";

/// The envelope answer for "there is nothing to fetch": `404`, the spec's one `not_found` code, and the
/// sentence above.
fn no_output() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, NOT_FOUND, NO_OUTPUT)
}

/// Answer one tool's stored output: 200 and the text, `404 not_found` otherwise.
pub(crate) async fn respond(state: &AppState, session_id: &str, tool_id: &str) -> Response {
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
        return no_output().into_response();
    }
    let store = Store::new(&state.config.data_root);
    match store.read(session_id, tool_id) {
        Some(bytes) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            bytes,
        )
            .into_response(),
        None => no_output().into_response(),
    }
}
