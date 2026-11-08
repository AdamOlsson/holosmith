//! The two routes a browser's subscription is stored through (HS-066, spec item 11).
//!
//! `POST /api/push/subscribe` stores one subscription; `DELETE /api/push/subscribe?endpoint=…` forgets one. Both
//! answer `204` on success and neither has a `GET`: there is nothing a client would do with the list, and a route
//! that enumerated the machine's subscribed devices would be a list of capability URLs.
//!
//! The refusals are ADR-0008's codes, and the split between them is the client's next step rather than the
//! transport status:
//!
//! | what is wrong | answer |
//! | --- | --- |
//! | the body is not `{endpoint, p256dh, auth}` | `422 bad_body` / `422 unknown_field` |
//! | the endpoint is not `https://host[:port]` | `422 bad_endpoint` |
//! | `p256dh` is not a 65-octet point, or `auth` is not 16 octets | `422 bad_subscription_key` |
//! | the `DELETE` names no endpoint | `422 bad_query` |
//! | the `DELETE` names one that is not stored | `404 not_found` |
//!
//! Both handlers are `async` only because the router's shape is: the work is three synchronous statements under one
//! lock, and the lock is never held across an `.await` (ADR-0012/ADR-0013). The upsert is an upsert because a
//! browser re-subscribing is the same endpoint with new keys, and `created_at` keeps the first value
//! ([`crate::push::repository`]).
//!
//! The query's `endpoint` is percent-decoded before it is compared, with the same reader `changes::path_of` uses —
//! an endpoint is full of `%2F`-able path segments, and a client that encoded them must not be told the row is
//! missing.

use std::sync::PoisonError;

use axum::body::Bytes;
use axum::extract::RawQuery;
use axum::extract::rejection::BytesRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::clock;
use crate::http::AppState;
use crate::http::changes::decode;
use crate::http::error::{ApiError, NOT_FOUND, internal};
use crate::push::jwt;
use crate::vapid;

/// The three fields a subscribe carries, and the whole of what it may carry.
const FIELDS: [&str; 3] = ["endpoint", "p256dh", "auth"];

/// The body's three strings, once they are known to be exactly that.
#[derive(Debug)]
struct Subscription {
    endpoint: String,
    p256dh: String,
    auth: String,
}

/// Store one subscription: `204`, or the sentence naming what is wrong with it.
pub async fn subscribe(state: &AppState, body: Result<Bytes, BytesRejection>) -> Response {
    let Ok(body) = body else {
        return ApiError::bad_subscription_body().into_response();
    };
    let subscription = match parse_body(&body) {
        Ok(subscription) => subscription,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = validate(&subscription) {
        return error.into_response();
    }
    let stored = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        database.push().upsert(
            &subscription.endpoint,
            &subscription.p256dh,
            &subscription.auth,
            clock::unix_now(),
        )
    };
    match stored {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal("push.upsert", error).into_response(),
    }
}

/// Forget one subscription: `204`, or `404` when the endpoint was never stored.
///
/// The endpoint travels in the query rather than in a path segment: it is an arbitrary URL with its own slashes
/// and its own escape sequences, and a path segment would be a value a router has to decode back into the thing the
/// client sent (`?force=true`'s neighbour on the delete route, and the spec's own `DELETE` shape).
pub async fn unsubscribe(state: &AppState, query: RawQuery) -> Response {
    let Some(endpoint) = endpoint_of(query.0.as_deref()) else {
        return ApiError::bad_query("the query must name the subscription's `endpoint`")
            .into_response();
    };
    let removed = {
        let database = state
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        database.push().remove(&endpoint)
    };
    match removed {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => ApiError::new(
            StatusCode::NOT_FOUND,
            NOT_FOUND,
            "there is no subscription with that endpoint",
        )
        .into_response(),
        Err(error) => internal("push.remove", error).into_response(),
    }
}

/// The body's three strings: a JSON object with exactly the accepted fields.
///
/// The `unknown_field` check runs before the type check so a body with a fourth key is told *that* rather than
/// being described as malformed (`http::sessions::create`'s `parse_body`, the same order and for the same reason).
fn parse_body(bytes: &[u8]) -> Result<Subscription, ApiError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| ApiError::bad_subscription_body())?;
    let object = value
        .as_object()
        .ok_or_else(ApiError::bad_subscription_body)?;
    if object.keys().any(|field| !FIELDS.contains(&field.as_str())) {
        return Err(ApiError::unknown_field());
    }
    let field = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(ApiError::bad_subscription_body)
    };
    Ok(Subscription {
        endpoint: field("endpoint")?,
        p256dh: field("p256dh")?,
        auth: field("auth")?,
    })
}

/// Whether the endpoint is one a notification can be sent to, and the two keys are RFC 8291's shapes.
///
/// The endpoint is checked before the keys so a browser that sent both wrong is told the thing whose fix comes
/// first: an `http://` origin is a client on a page that cannot push at all, and a wrong `p256dh` is one whose
/// `getSubscription()` is the next step.
fn validate(subscription: &Subscription) -> Result<(), ApiError> {
    // The origin is what the JWT's `aud` will be, so "this value has no origin" is the whole check; there is
    // nothing else to compare it with here.
    if jwt::origin(&subscription.endpoint).is_none() {
        return Err(ApiError::bad_endpoint(
            "`endpoint` must be an https URL with a host: `https://host[:port]` and nothing else, because a push \
             service sends to the origin and a page served over http cannot subscribe at all",
        ));
    }
    match vapid::unbase64url(&subscription.p256dh) {
        Some(point) if point.len() == 65 && point[0] == 0x04 => {}
        _ => {
            return Err(ApiError::bad_subscription_key(
                "`p256dh` must be an unpadded base64url of the 65 octets of an uncompressed P-256 point, which \
                 is what a browser's `getSubscription()` carries",
            ));
        }
    }
    match vapid::unbase64url(&subscription.auth) {
        Some(auth) if auth.len() == 16 => {}
        _ => {
            return Err(ApiError::bad_subscription_key(
                "`auth` must be an unpadded base64url of the 16 octets RFC 8291 §3.2 asks for, which is what a \
                 browser's `getSubscription()` carries",
            ));
        }
    }
    Ok(())
}

/// The query's `endpoint`, decoded; `None` when it is missing or empty.
///
/// An empty value is `bad_query` rather than `not_found`: "no endpoint" is a request that did not name a row, while
/// `not_found` says the row this request named is not there.
fn endpoint_of(query: Option<&str>) -> Option<String> {
    let mut named: Option<String> = None;
    for pair in query.unwrap_or_default().split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name == "endpoint" {
            named = Some(decode(value));
        }
    }
    named.filter(|endpoint| !endpoint.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_is_exactly_three_strings() {
        let body = br#"{"endpoint":"https://push.example/one","p256dh":"BAEC","auth":"ERER"}"#;
        let subscription = parse_body(body).expect("the three fields are there");
        assert_eq!(subscription.endpoint, "https://push.example/one");
        assert_eq!(subscription.p256dh, "BAEC");
        assert_eq!(subscription.auth, "ERER");
        // A fourth field is `unknown_field`, not `bad_body`: the client sent something this endpoint does not know.
        let extra =
            br#"{"endpoint":"https://push.example/one","p256dh":"BAEC","auth":"ERER","keys":{}}"#;
        assert_eq!(
            parse_body(extra).expect_err("a fourth field").body.code,
            crate::http::error::UNKNOWN_FIELD
        );
        // A missing field, a non-string field and a body that is not an object are all `bad_body`.
        for malformed in [
            &br#"{"p256dh":"BAEC","auth":"ERER"}"#[..],
            &br#"{"endpoint":1,"p256dh":"BAEC","auth":"ERER"}"#[..],
            &br#"["endpoint"]"#[..],
            &b"not json"[..],
        ] {
            assert_eq!(
                parse_body(malformed)
                    .expect_err("not a subscribe")
                    .body
                    .code,
                crate::http::error::BAD_BODY
            );
        }
    }

    #[test]
    fn an_endpoint_is_an_https_origin_and_the_keys_are_rfc_8291_shapes() {
        let point = vapid::base64url(&[0x04; 65]);
        let auth = vapid::base64url(&[0; 16]);
        let good = Subscription {
            endpoint: "https://push.example/one?two".to_string(),
            p256dh: point.clone(),
            auth: auth.clone(),
        };
        assert!(validate(&good).is_ok());
        for endpoint in ["http://push.example/one", "https:///one", ""] {
            let subscription = Subscription {
                endpoint: endpoint.to_string(),
                p256dh: point.clone(),
                auth: auth.clone(),
            };
            assert_eq!(
                validate(&subscription).expect_err(endpoint).body.code,
                crate::http::error::BAD_ENDPOINT,
                "{endpoint}"
            );
        }
        // A 64-octet point and a point that is not uncompressed.
        for wrong in [vapid::base64url(&[0x04; 64]), vapid::base64url(&[0x02; 65])] {
            let subscription = Subscription {
                endpoint: good.endpoint.clone(),
                p256dh: wrong.clone(),
                auth: auth.clone(),
            };
            assert_eq!(
                validate(&subscription).expect_err(&wrong).body.code,
                crate::http::error::BAD_SUBSCRIPTION_KEY
            );
        }
        // A 15-octet auth secret, and one that is not base64url at all.
        for wrong in [vapid::base64url(&[0; 15]), "not base64url!".to_string()] {
            let subscription = Subscription {
                endpoint: good.endpoint.clone(),
                p256dh: point.clone(),
                auth: wrong.clone(),
            };
            assert_eq!(
                validate(&subscription).expect_err(&wrong).body.code,
                crate::http::error::BAD_SUBSCRIPTION_KEY
            );
        }
    }

    #[test]
    fn the_query_names_the_endpoint_and_is_decoded() {
        let endpoint = "https://push.example/a%2Fb?c=d";
        assert_eq!(
            endpoint_of(Some(&format!("endpoint={endpoint}"))).as_deref(),
            Some("https://push.example/a/b?c=d")
        );
        // A missing endpoint, an empty one and a query that is not one are all `None`.
        assert_eq!(endpoint_of(None), None);
        assert_eq!(endpoint_of(Some("endpoint=")), None);
        assert_eq!(endpoint_of(Some("other=x")), None);
        // An invalid escape stays literal, as `changes::path_of`'s does: the value is compared, never joined.
        assert_eq!(
            endpoint_of(Some("endpoint=https://push.example/%zz")).as_deref(),
            Some("https://push.example/%zz")
        );
    }
}
