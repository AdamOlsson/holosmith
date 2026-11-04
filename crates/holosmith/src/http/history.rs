//! `GET /api/sessions/:id/events/history`: one older page of a session's log (HS-061).
//!
//! The stream carries the newest messages and says where it stopped (`history.truncated`); this
//! endpoint answers what came before that. It is the second half of one rule — the cursor a
//! truncation frame names is exactly the `before` this route takes — so the two are written together
//! and neither invents a cursor of its own.
//!
//! A `GET`, so every guard applies exactly as it does to the other read-only routes and nothing new
//! is needed for it (ADR-0010). The page is one repository read: `seq < before ORDER BY seq DESC
//! LIMIT n`, reversed. Seq is strictly increasing, so an event committed after the read has a seq
//! above every row a page can hold and cannot shift its content; the only thing that moves a page is
//! compaction removing rows, which is the log's documented gap rule (ADR-0032).

use std::sync::PoisonError;

use axum::Json;
use axum::extract::RawQuery;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::events::Event;
use crate::http::app::AppState;
use crate::http::error::{ApiError, internal};

/// How many events a page carries when the request does not say.
///
/// The story's own number. Large enough that a phone paging back through a long turn needs a handful
/// of taps, small enough that the answer is one response rather than a stream.
const DEFAULT_LIMIT: u32 = 200;

/// The largest page a request may ask for. A ceiling rather than a refusal: a caller asking for more
/// than this gets this, because "give me everything" is a paging mistake and not a malformed request.
const MAX_LIMIT: u32 = 1000;

/// What one history request asks for: the exclusive cursor, and the page size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
    before: i64,
    limit: u32,
}

/// The response: one page, the cursor for the next one, and whether there is one.
///
/// `before_seq` is the page's **lowest** seq — the value to pass as the next `before` — so a client
/// that follows the frame's cursor and then this one never has to compute a seq itself. `turns_before`
/// is how many turn boundaries sit below it, which is what lets a client fold the page with the right
/// turn numbers even though it never saw those boundaries.
#[derive(Debug, Serialize)]
struct Page<'a> {
    events: Vec<PageEvent<'a>>,
    before_seq: i64,
    has_more: bool,
    turns_before: u32,
}

/// One event as this endpoint spells it: the row's four fields, without the session id the path
/// already names.
#[derive(Debug, Serialize)]
struct PageEvent<'a> {
    seq: i64,
    #[serde(rename = "type")]
    kind: &'a str,
    payload: &'a str,
    created_at: i64,
}

impl<'a> PageEvent<'a> {
    fn of(event: &'a Event) -> Self {
        Self {
            seq: event.seq,
            kind: &event.kind,
            payload: &event.payload,
            created_at: event.created_at,
        }
    }
}

/// Answer a page of history: 200 and the page, ADR-0008's envelope otherwise.
pub(crate) async fn respond(state: &AppState, session_id: &str, query: RawQuery) -> Response {
    let cursor = match parse_query(query.0.as_deref()) {
        Ok(cursor) => cursor,
        Err(error) => return error.into_response(),
    };
    // One synchronous scope, released before the answer is built: the lock is never held across an
    // await (ADR-0013). The row and the page are read under the same lock, so a session deleted
    // between the two cannot answer a page of a log that has gone.
    let (events, before_seq, has_more, turns_before) = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match database.sessions().get(session_id) {
            Ok(Some(_)) => {}
            Ok(None) => return ApiError::unknown_session().into_response(),
            Err(error) => return internal("get", error).into_response(),
        }
        let repository = database.events();
        // One row more than the page: whether that row exists is exactly what `has_more` means, and
        // counting the rest would be a second statement for the same answer.
        let mut events =
            match repository.read_before(session_id, cursor.before, cursor.limit.saturating_add(1))
            {
                Ok(events) => events,
                Err(error) => return internal("read_before", error).into_response(),
            };
        let has_more = events.len() > usize::try_from(cursor.limit).unwrap_or(usize::MAX);
        if has_more {
            // `read_before` answers in seq order, so the row past the page is the oldest one.
            events.remove(0);
        }
        // An empty page keeps the cursor it was asked for: there is nothing below it, so "before the
        // same place" is the honest next question.
        let before_seq = events.first().map_or(cursor.before, |event| event.seq);
        let turns_before = match repository.turns_before(session_id, before_seq) {
            Ok(count) => count,
            Err(error) => return internal("turns_before", error).into_response(),
        };
        (events, before_seq, has_more, turns_before)
    };
    let page = Page {
        events: events.iter().map(PageEvent::of).collect(),
        before_seq,
        has_more,
        turns_before,
    };
    (StatusCode::OK, Json(page)).into_response()
}

/// Read `before` and `limit` out of the query, or refuse the request (HS-061).
///
/// `before` is **required**: a client that pages history without naming where to start is asking for
/// the newest page, which is what the stream already gave it. Every other parameter is ignored rather
/// than refused — the delete's `?force=true` reads its query the same lenient way, and a page's size is
/// the only other thing this route can mean.
fn parse_query(query: Option<&str>) -> Result<Cursor, ApiError> {
    let mut before: Option<i64> = None;
    let mut limit: Option<u32> = None;
    for pair in query.unwrap_or_default().split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        match name {
            "before" => {
                before = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| ApiError::bad_query(BEFORE))?,
                )
            }
            "limit" => {
                limit = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| ApiError::bad_query(LIMIT))?,
                )
            }
            _ => {}
        }
    }
    let Some(before) = before.filter(|seq| *seq > 0) else {
        return Err(ApiError::bad_query(BEFORE));
    };
    Ok(Cursor {
        before,
        limit: limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
    })
}

/// What a `before` that is not a positive seq is told. The sentence names where a good value comes
/// from, because that is the whole difficulty of this cursor.
const BEFORE: &str = "before must be a positive seq: pass the before_seq a history.truncated frame or \
                      a history page named";

/// What a `limit` that is not a positive integer is told.
const LIMIT: &str = "limit must be a positive integer, at most 1000";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_before_and_a_limit_are_read_from_the_query() {
        assert_eq!(
            parse_query(Some("before=41&limit=10")),
            Ok(Cursor {
                before: 41,
                limit: 10
            })
        );
        assert_eq!(
            parse_query(Some("before=41")),
            Ok(Cursor {
                before: 41,
                limit: DEFAULT_LIMIT
            })
        );
    }

    #[test]
    fn a_page_is_never_bigger_than_the_ceiling_and_never_empty() {
        assert_eq!(
            parse_query(Some("before=7&limit=100000"))
                .expect("a large limit is clamped")
                .limit,
            MAX_LIMIT
        );
        assert_eq!(
            parse_query(Some("before=7&limit=0"))
                .expect("a zero limit is clamped")
                .limit,
            1
        );
    }

    #[test]
    fn before_is_required_and_must_be_a_positive_seq() {
        // Absent, zero, negative and non-numeric are one answer, because none of them names a place in
        // the log: the stream's cursor is the only thing that does.
        for query in [
            None,
            Some(""),
            Some("limit=5"),
            Some("before=0"),
            Some("before=-3"),
            Some("before=abc"),
        ] {
            assert_eq!(
                parse_query(query).map(|cursor| cursor.before),
                Err(ApiError::bad_query(BEFORE)),
                "query {query:?} was accepted as a cursor"
            );
        }
    }

    #[test]
    fn a_limit_that_is_not_an_integer_is_refused_rather_than_ignored() {
        for query in ["before=7&limit=abc", "before=7&limit=-1", "before=7&limit="] {
            assert_eq!(
                parse_query(Some(query)).map(|cursor| cursor.limit),
                Err(ApiError::bad_query(LIMIT)),
                "query {query:?} was accepted as a page size"
            );
        }
    }

    #[test]
    fn an_unknown_parameter_is_ignored() {
        assert_eq!(
            parse_query(Some("before=9&since=3&limit=2")),
            Ok(Cursor {
                before: 9,
                limit: 2
            })
        );
    }
}
