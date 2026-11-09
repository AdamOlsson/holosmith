//! `ANY /api/hosts/{name}/{*rest}`: one machine's call, relayed to another (HS-069, spec item 3, ADR-0074).
//!
//! The spec gives the hub the `/api/hosts*` prefix ("a hub serves `/api/hosts*`, a server serves the session API")
//! and says API paths are unchanged on every role, so a forwarded call is the peer's own path under this machine's
//! host prefix: `/api/hosts/workshop/sessions/<id>/prompt` reaches `https://workshop…/api/sessions/<id>/prompt`. One
//! wildcard route rather than a second registration of every session route, and the peer's paths stay its own.
//!
//! **Five registrations of one path, not one `any` route.** `Routes` records a method and a kind per entry
//! (ADR-0010), and a forwarded `GET` cannot change state *at this machine* — so a `GET` is `ReadOnly` and the four
//! mutating verbs are `Mutating`, which is what the manifest can honestly say. The method that goes on the wire is
//! the client's, unmapped.
//!
//! The order of the decisions is the interesting part:
//!
//! 1. **`local` is this machine**, and it is a redirect rather than a hop: `308` to `/api/<rest>`, bodyless and
//!    path-relative, decided **before** the row lookup (no row may be named `local`). One path shape for a client —
//!    `/api/hosts/<host>/…` for every host, the local one included — without this machine dialling itself.
//! 2. **`hosts` is not relayed.** The registry is each machine's own (`GET`/`POST /api/hosts` are *this* machine's
//!    table), and a relayed registration would be a second-hand claim about a third machine — ADR-0016's distrust,
//!    one hop out. One hop is the whole promise, and this is what makes it structural.
//! 3. **The row names the peer, or there is no call.** An unknown name is `404 unknown_host`; the row's `url` is the
//!    origin everything is sent to. Reachability is *not* consulted: it is a fact about the last probe, and a
//!    machine that was asleep and is now awake deserves the request.
//! 4. **The body is buffered and the head is bounded.** Every body this API takes is small JSON, and the bound
//!    ([`crate::forward::Forwarding::timeout`]) covers the dial, the request and the reply's **head** — never the
//!    body, because an event stream has no end.
//! 5. **The peer's answer is the answer**: its status, its headers (minus the hop-by-hop ones) and its body, relayed.
//!    A refusal from the peer therefore arrives as the peer wrote it, envelope and all.

use std::sync::PoisonError;

use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::forward::{self, Failure};
use crate::hosts::{self, Host};
use crate::http::AppState;
use crate::http::error::{ApiError, internal};

/// The most of a client's body this route carries. Axum's own default for a buffered body, and the reason it is
/// buffered at all: `Content-Length` is recomputed from the bytes, so the peer's framing is this machine's to state
/// rather than the client's to guess.
const BODY_LIMIT: usize = 2 * 1024 * 1024;

/// The one segment a hub will not relay: the registry is each machine's own.
const REGISTRY: &str = "hosts";

/// Answer one forwarded call.
pub(crate) async fn respond(
    state: &AppState,
    path: Result<Path<(String, String)>, PathRejection>,
    request: Request,
) -> Response {
    // A path segment that is not UTF-8 is a name this machine cannot look up, which is `not_found` and not a
    // `text/plain` rejection: the house rule for every `{id}` in this table.
    let Ok(Path((name, _))) = path else {
        return ApiError::not_found().into_response();
    };
    // **The tail is taken raw.** `Path` percent-decodes, so the decoded `rest` could carry a CRLF or a space that
    // this machine would write into the peer's request line — a caller authoring the head the module claims the hub
    // authors. The bytes the client sent, positioned past `/api/hosts/<name>/`, are the ones the peer should see.
    let Some(rest) = forward::raw_rest(request.uri().path()).map(str::to_string) else {
        return ApiError::not_found().into_response();
    };
    // Defence in depth for that rule rather than a second rule: a request target cannot legally contain a control
    // byte, and one that arrived somehow never becomes part of a head this machine writes.
    if rest.bytes().any(|byte| byte < 0x21 || byte == 0x7f) {
        return ApiError::not_found().into_response();
    }
    if name == hosts::LOCAL {
        return redirect(&rest, request.uri().query());
    }
    if rest.split('/').next() == Some(REGISTRY) {
        return ApiError::not_forwarded(
            "the host registry is each machine's own, so a hub does not relay /api/hosts: register the machine\n \
             where it lives, with POST /api/hosts on that machine",
        )
        .into_response();
    }
    let host = match read_row(state, &name) {
        Ok(Some(host)) => host,
        Ok(None) => return ApiError::unknown_host(name).into_response(),
        Err(error) => return error.into_response(),
    };
    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(bytes) => bytes,
        // The same code as the create's refusal and **this** request's sentence: a client told about a create's four
        // fields would be describing the wrong request (ADR-0008, `bad_prompt`'s precedent).
        Err(_) => return ApiError::bad_forward_body().into_response(),
    };
    let method = parts.method.as_str().to_string();
    let head = match forward::request_head(&forward::Request {
        method: &method,
        rest: &rest,
        query: parts.uri.query(),
        origin: &host.url,
        headers: &parts.headers,
        length: body.len(),
    }) {
        Some(head) => head,
        None => {
            return ApiError::upstream_unreachable(format!(
                "the registry's row for {} is not an origin this machine can call",
                host.name
            ))
            .into_response();
        }
    };
    let deadline = tokio::time::Instant::now() + state.forwarding.timeout(&method, &rest);
    let mut link = match forward::Link::connect(&host.url, &state.tools).await {
        Ok(link) => link,
        Err(failure) => return refusal(&host, &failure),
    };
    if let Err(failure) = link.send(&head, &body, deadline).await {
        return refusal(&host, &failure);
    }
    let (reply, early) = match link.read_reply(deadline).await {
        Ok(answer) => answer,
        Err(failure) => return refusal(&host, &failure),
    };
    // A `HEAD` answer has no body to relay, and the head already says what a `GET` would: the stream is dropped
    // (which ends the hop, and kills a TLS child) rather than polled for bytes the peer will never send.
    let relay = if parts.method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(forward::BodyStream::new(link, reply.framing, early))
    };
    let mut response = Response::new(relay);
    *response.status_mut() = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::BAD_GATEWAY);
    for (name, value) in reply.headers {
        if !forward::keep_header(&name) {
            continue;
        }
        // A peer that sends **both** `Content-Length` and `Transfer-Encoding: chunked` is a peer this build's own
        // hyper never is, and the decoded body's length is not the one that header names: copying it would make
        // hyper promise octets the answer does not have. The framing wins, and a length is copied only when it is
        // the framing.
        if name.eq_ignore_ascii_case("content-length")
            && !matches!(reply.framing, forward::Framing::Length(length) if value.trim().parse::<u64>().ok() == Some(length))
        {
            continue;
        }
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) else {
            // A header this machine cannot spell is one a client could not have sent either; dropping it is the
            // only answer that keeps the rest of the peer's reply intact (ADR-0008's "never a partial envelope"
            // does not apply to a reply this machine did not author).
            continue;
        };
        response.headers_mut().append(name, value);
    }
    tracing::debug!(
        target: "holosmith::forward",
        host = %host.name,
        method = %parts.method,
        rest = %rest,
        status = reply.status,
        "forwarded"
    );
    response
}

/// The row this call is addressed to, read under one lock scope and dropped before anything awaits.
fn read_row(state: &AppState, name: &str) -> Result<Option<Host>, ApiError> {
    let database = state
        .database
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    database
        .hosts()
        .one(name)
        .map_err(|error| internal("hosts.one", error))
}

/// `/api/hosts/local/<rest>` → `/api/<rest>`: the machine the client is already talking to.
///
/// A `308` because the method and the body must survive it (a forwarded `POST` to `local` is a `POST` to this
/// machine), bodyless because a redirect has nothing to say, and **not** ADR-0008's envelope because a redirect is
/// not a refusal. It is answered before the row lookup, like every other machine fact on this path.
fn redirect(rest: &str, query: Option<&str>) -> Response {
    let mut location = format!("/api/{rest}");
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        location.push('?');
        location.push_str(query);
    }
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::PERMANENT_REDIRECT;
    let value =
        HeaderValue::from_str(&location).unwrap_or_else(|_| HeaderValue::from_static("/api/"));
    response.headers_mut().insert(header::LOCATION, value);
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    response
}

/// A failed hop, as the code that describes it: a timeout is `504`, everything else is `502`.
fn refusal(host: &Host, failure: &Failure) -> Response {
    let sentence = failure.sentence(&host.name, &host.url);
    tracing::warn!(
        target: "holosmith::forward",
        host = %host.name,
        origin = %host.url.as_str(),
        %sentence,
        "a forwarded call did not reach its peer"
    );
    match failure {
        Failure::Timeout => ApiError::upstream_timeout(sentence).into_response(),
        Failure::Sentence(_) => ApiError::upstream_unreachable(sentence).into_response(),
    }
}
