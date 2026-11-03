//! `GET /api/sessions/:id/events`: one session's event stream (HS-010, ADR-0014).
//!
//! A `GET`, so every guard applies exactly as it does to the other read-only routes and nothing
//! new is needed for it (ADR-0010). The body is built in [`crate::events::stream`]; this file
//! decides the two things the request carries — which session, and where the replay starts — and
//! the keep-alive interval axum writes with.

use std::sync::PoisonError;
use std::time::Duration;

use axum::http::HeaderMap;
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};

use crate::events::stream;
use crate::http::app::AppState;
use crate::http::error::{ApiError, internal};

/// The header a reconnecting client carries its last-seen seq in. A browser sets it from the
/// last SSE `id` it saw, which is the seq (D1).
const LAST_EVENT_ID: &str = "last-event-id";

/// How long the stream may stay silent before axum writes its keep-alive comment line.
///
/// The comment is axum's [`KeepAlive`], configured here rather than written by hand: the line
/// `:\n\n` is emitted by the same layer that knows the stream is idle, and a second hand-written
/// comment would write it twice. Named, and re-exported from `http`, so a test can assert the
/// interval without waiting it out.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Answer a client that opens the stream: `text/event-stream` for a known session, ADR-0008's
/// envelope otherwise.
pub(crate) async fn respond(state: &AppState, session_id: &str, headers: &HeaderMap) -> Response {
    match known(state, session_id) {
        Ok(true) => {}
        Ok(false) => return ApiError::not_found().into_response(),
        Err(error) => return internal("get", error).into_response(),
    }
    let after = resume_point(headers);
    let body = match stream::attach(
        &state.events,
        &state.database,
        &state.streams,
        session_id,
        after,
    ) {
        Ok(body) => body,
        // Nothing has been sent yet, so this is still a plain HTTP answer.
        Err(error) => return internal("read_since", error).into_response(),
    };
    Sse::new(body)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE_INTERVAL))
        .into_response()
}

/// Whether the session exists. Streams are never opened for a row that does not, and the check
/// runs before the stream registry is touched, so an unknown id registers nothing.
fn known(state: &AppState, session_id: &str) -> Result<bool, crate::sessions::RepoError> {
    let database = state
        .database
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    Ok(database.sessions().get(session_id)?.is_some())
}

/// Where the replay starts: the seq after the client's last-seen one, or the beginning.
///
/// Absent, empty, non-numeric and non-positive all mean "from the beginning", which is what this
/// story promises a new client (D4). HS-061 replaces that with "the 50th most recent message"
/// and its `history.truncated` event; no truncation rule is invented here.
fn resume_point(headers: &HeaderMap) -> i64 {
    headers
        .get(LAST_EVENT_ID)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<i64>().ok())
        .filter(|seq| *seq > 0)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(value: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(value) = value {
            headers.insert(
                LAST_EVENT_ID,
                HeaderValue::from_str(value).expect("a test header value is valid"),
            );
        }
        headers
    }

    #[test]
    fn a_numeric_last_event_id_resumes_after_it() {
        assert_eq!(resume_point(&headers(Some("7"))), 7);
        assert_eq!(resume_point(&headers(Some("  7 "))), 7);
        assert_eq!(resume_point(&headers(Some("0"))), 0);
    }

    #[test]
    fn anything_else_starts_at_the_beginning() {
        for value in [
            None,
            Some(""),
            Some(" "),
            Some("abc"),
            Some("-3"),
            Some("1.5"),
        ] {
            assert_eq!(
                resume_point(&headers(value)),
                0,
                "Last-Event-ID {value:?} did not start at the beginning"
            );
        }
    }

    #[test]
    fn the_keep_alive_is_axums_fifteen_seconds() {
        // A test that waited the interval out would take fifteen seconds and prove less: what
        // matters is that the configured interval is the spec's, and that no second keep-alive
        // line is written by hand (there is no `Event::comment` call in this crate).
        assert_eq!(KEEP_ALIVE_INTERVAL, Duration::from_secs(15));
    }
}
