//! The host registry's three routes (spec item 4).
//!
//! `GET /api/hosts` answers the table, `POST /api/hosts` registers one machine, and `DELETE /api/hosts/{name}`
//! forgets one. All three are the registry's own rows and nothing else: no route here forwards a request (
//! owns that), and none of them authenticates a peer.
//!
//! The refusals are ADR-0008's codes, and the split between them is the client's next step rather than the transport
//! status:
//!
//! | what is wrong | answer |
//! | --- | --- |
//! | the body is not `{name, url}` | `422 bad_body` / `422 unknown_field` |
//! | the name is not one a row may have, or it is `local` | `422 bad_host_name` |
//! | the URL is not an origin, its host is not tailnet, its port is out of range | `422 bad_host_url` |
//! | the host answered, and not with a health payload | `422 not_holosmith` |
//! | the host answered nothing | **not a refusal**: `201`, with the row `unreachable` and the sentence |
//! | the `DELETE` names no row | `404 not_found` |
//! | the `DELETE` names this machine, which has live sessions | `409 host_busy`, unless `?force=true` |
//!
//! **The probe runs off the request thread and outside the database lock.** It leaves the machine and can take
//! seconds, so it goes to `spawn_blocking` with the config cloned, and the row is written under one lock scope
//! afterwards (ADR-0012/ADR-0013's rule: a guard never spans an `.await`, and here it does not span a socket either).
//!
//! **`POST` is an upsert on the name.** The spec names no conflict code, a machine registered twice is the operator
//! retrying, and the seed's "the file wins" is the same statement — one write path for both, which is also why the
//! route answers `201` for a row that replaced an older one.

use std::sync::PoisonError;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{Path, RawQuery};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;

use crate::hosts::{self, Host, Refusal};
use crate::http::AppState;
use crate::http::error::{ApiError, NOT_FOUND, internal};
use crate::http::sessions::force_requested;

/// The two fields a registration carries, and the whole of what it may carry.
const FIELDS: [&str; 2] = ["name", "url"];

/// The table as `GET /api/hosts` answers it, and as `holosmith hub host list` reads it back.
///
/// One key, and a wrapper rather than a bare array for `SessionList`'s reason: the sessions list already carries
/// facts about the machine beside its rows, and a host list is the shape a second fact joins without moving the first.
/// It derives **both** halves because the CLI is a client of this route: a second struct for the read side would
/// be a second spelling of one wire shape.
#[derive(Debug, Serialize, serde::Deserialize)]
pub struct HostList {
    pub hosts: Vec<Host>,
}

/// The body's two strings, once they are known to be exactly that.
#[derive(Debug)]
struct Registration {
    name: String,
    url: String,
}

/// Answer `GET /api/hosts`: every row, by name.
pub async fn list(state: &AppState) -> Response {
    let hosts = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        database.hosts().all()
    };
    match hosts {
        Ok(hosts) => Json(HostList { hosts }).into_response(),
        Err(error) => internal("hosts.list", error).into_response(),
    }
}

/// Answer `POST /api/hosts`: the rules, the probe, the row, or the sentence saying why not.
pub async fn register(state: &AppState, body: Result<Bytes, BytesRejection>) -> Response {
    let Ok(body) = body else {
        return ApiError::bad_host_body().into_response();
    };
    let registration = match parse_body(&body) {
        Ok(registration) => registration,
        Err(error) => return error.into_response(),
    };
    // The probe leaves the machine, so it is off this thread and holds nothing: the config and the tools it needs
    // are cheap clones, and the database is reached again only to write what the probe measured.
    let config = state.config.clone();
    let tools = state.tools.clone();
    let Registration { name, url } = registration;
    let probed =
        tokio::task::spawn_blocking(move || hosts::register(&config, &tools, &name, &url)).await;
    let row = match probed {
        Ok(Ok(row)) => row,
        Ok(Err(refusal)) => return refusal_of(&refusal).into_response(),
        // A task that panicked has said nothing about the host, and that is not a refusal — it is this machine's
        // problem, which `internal` answers and logs once (ADR-0008).
        Err(error) => return internal("hosts.register", error).into_response(),
    };
    let stored = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        database.hosts().upsert(&row)
    };
    match stored {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        Err(error) => internal("hosts.upsert", error).into_response(),
    }
}

/// Answer `DELETE /api/hosts/{name}`: `204`, `404`, or the `409` that `?force=true` lifts.
///
/// Spec item 4's "removing a host is refused while it has live sessions" is asked of the **local** row — the one whose
/// origin is this machine's own — because a *remote* row's sessions are not this machine's to count. That is the one
/// reading this build can answer honestly, and the
/// ADR says so rather than pretending a hub can see a server's rows.
pub async fn remove(state: &AppState, name: &str, query: RawQuery) -> Response {
    // The query is read before the row: a typo in `?force` must not remove anything it was not forced to.
    let force = force_requested(query.0.as_deref());
    let (row, live) = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let row = match database.hosts().one(name) {
            Ok(Some(row)) => row,
            Ok(None) => return not_found(name).into_response(),
            Err(error) => return internal("hosts.one", error).into_response(),
        };
        let live = if !force && hosts::is_local(&row.url, &state.config.control_origin) {
            match database.sessions().live_count() {
                Ok(live) => live,
                Err(error) => return internal("hosts.live_count", error).into_response(),
            }
        } else {
            0
        };
        (row, live)
    };
    if live > 0 {
        return ApiError::host_busy(format!(
            "{:?} is this machine, and it has {} live session{}; removing the row does not stop {} — send \
             `?force=true` to remove it anyway",
            row.name,
            live,
            if live == 1 { "" } else { "s" },
            if live == 1 { "it" } else { "them" },
        ))
        .into_response();
    }
    let removed = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        database.hosts().remove(name)
    };
    match removed {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        // The row was there a moment ago: a second request removed it between the two lock scopes, and "it is gone"
        // is a truer answer than a `500` about a race nobody can see.
        Ok(false) => not_found(name).into_response(),
        Err(error) => internal("hosts.remove", error).into_response(),
    }
}

/// The body's two strings: a JSON object with exactly the accepted fields.
///
/// The `unknown_field` check runs before the type check so a body with a third key is told *that* rather than being
/// described as malformed (`http::push::parse_body`, the same order and for the same reason).
fn parse_body(bytes: &[u8]) -> Result<Registration, ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ApiError::bad_host_body())?;
    let object = value.as_object().ok_or_else(ApiError::bad_host_body)?;
    if object.keys().any(|field| !FIELDS.contains(&field.as_str())) {
        return Err(ApiError::unknown_field());
    }
    let field = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(ApiError::bad_host_body)
    };
    Ok(Registration {
        name: field("name")?,
        url: field("url")?,
    })
}

/// The registry's refusal as the envelope's code.
fn refusal_of(refusal: &Refusal) -> ApiError {
    match refusal {
        Refusal::Name(sentence) => ApiError::bad_host_name(sentence.clone()),
        Refusal::Url(sentence) => ApiError::bad_host_url(sentence.clone()),
        Refusal::NotHolosmith(sentence) => ApiError::not_holosmith(sentence.clone()),
    }
}

/// The one sentence a `DELETE` of a name the table does not hold answers, for both of its moments.
fn not_found(name: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        NOT_FOUND,
        format!("there is no host named {name:?}"),
    )
}

/// One route's path segment, read as a name: a `%FF` segment is a name the table does not hold.
///
/// The same treatment its neighbours on `/api/sessions` get (a `Result<Path<String>, PathRejection>` mapped to an
/// empty string), because a rejection left to axum answers `text/plain` outside ADR-0008's envelope.
pub fn path_name(path: Result<Path<String>, PathRejection>) -> String {
    path.map(|Path(name)| name).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_is_exactly_two_strings() {
        let body = br#"{"name":"workshop","url":"https://workshop.tail1234.ts.net"}"#;
        let registration = parse_body(body).expect("the two fields are there");
        assert_eq!(registration.name, "workshop");
        assert_eq!(registration.url, "https://workshop.tail1234.ts.net");
        // A third field is `unknown_field`, not `bad_body`: the client sent something this endpoint does not know.
        let extra = br#"{"name":"a","url":"b","role":"server"}"#;
        assert_eq!(
            parse_body(extra).expect_err("a third field").body.code,
            crate::http::error::UNKNOWN_FIELD
        );
        for malformed in [
            &br#"{"name":"a"}"#[..],
            &br#"{"name":1,"url":"b"}"#[..],
            &br#"["name"]"#[..],
            &b"not json"[..],
        ] {
            assert_eq!(
                parse_body(malformed)
                    .expect_err("not a registration")
                    .body
                    .code,
                crate::http::error::BAD_BODY
            );
        }
    }

    #[test]
    fn each_refusal_is_its_own_code_and_status() {
        // The **status** as well as the code: a `422` turned into a `200` would leave a client with no way to tell a
        // stored registration from a refused one, and a test that compares only the code stays green.
        for (refusal, code, status) in [
            (
                Refusal::Name("bad name".to_string()),
                crate::http::error::BAD_HOST_NAME,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                Refusal::Url("bad url".to_string()),
                crate::http::error::BAD_HOST_URL,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                Refusal::NotHolosmith("not one of ours".to_string()),
                crate::http::error::NOT_HOLOSMITH,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let error = refusal_of(&refusal);
            assert_eq!(error.status, status, "{code}");
            assert_eq!(error.body.code, code);
            assert_eq!(error.body.message, refusal.sentence());
        }
        // The fourth code is the delete's, and its status is a `409`: the request was well formed and something live
        // is in the way.
        let busy = ApiError::host_busy("one live session");
        assert_eq!(busy.status, StatusCode::CONFLICT);
        assert_eq!(busy.body.code, crate::http::error::HOST_BUSY);
    }
}
