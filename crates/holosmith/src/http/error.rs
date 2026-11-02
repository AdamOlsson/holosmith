//! One JSON error envelope for every error that has a body (ADR-0008).
//!
//! `{"code": "<machine code>", "message": "<one human sentence>"}`. The code is a stable
//! snake_case name from the spec, never the transport status; the message may be rewritten
//! without breaking a client.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// An unknown path under `/api/`.
pub const NOT_FOUND: &str = "not_found";

/// The `Host` header is not one this machine answers (ADR-0010).
pub const BAD_HOST: &str = "bad_host";

/// A request that changes something did not come from the control origin (ADR-0010).
pub const BAD_ORIGIN: &str = "bad_origin";

/// A request that changes something is not the Holosmith client (ADR-0010).
pub const BAD_CLIENT: &str = "bad_client";

/// The tailnet user behind the request is not the configured owner (ADR-0010).
pub const BAD_OWNER: &str = "bad_owner";

/// A create's body is not a JSON object with the four accepted fields (HS-008).
pub const BAD_BODY: &str = "bad_body";

/// A create's body carries a field the endpoint does not accept (HS-008).
pub const UNKNOWN_FIELD: &str = "unknown_field";

/// A create did not carry an `Idempotency-Key` header (HS-008).
pub const MISSING_IDEMPOTENCY_KEY: &str = "missing_idempotency_key";

/// A create's `Idempotency-Key` is not one value of at most 200 characters (HS-008).
pub const BAD_IDEMPOTENCY_KEY: &str = "bad_idempotency_key";

/// A create's `project` is not one path segment (HS-008).
pub const BAD_PROJECT: &str = "bad_project";

/// A create's `model` is not one model name (HS-008).
pub const BAD_MODEL: &str = "bad_model";

/// A create's `thinking` is not one thinking level (HS-008).
pub const BAD_THINKING: &str = "bad_thinking";

/// A create was refused because the machine already runs the maximum number of live sessions
/// (HS-008; HS-020 refines the branch with eviction).
pub const CAP_REACHED: &str = "cap_reached";

/// A create could not be completed for a reason no client can fix (HS-008).
pub const INTERNAL: &str = "internal";

/// The body of every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: &'static str,
}

/// A status and its envelope, ready to answer with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: ErrorBody,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            body: ErrorBody { code, message },
        }
    }

    /// What an unknown path under `/api/` answers.
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, NOT_FOUND, "no such endpoint")
    }

    /// What a request whose `Host` is not in the allow-list answers.
    pub fn bad_host() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            BAD_HOST,
            "the Host header is not one this machine answers",
        )
    }

    /// What a non-`GET` request from another origin answers.
    pub fn bad_origin() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            BAD_ORIGIN,
            "a request that changes something must come from the control origin",
        )
    }

    /// What a non-`GET` request without the JSON client header answers.
    pub fn bad_client() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            BAD_CLIENT,
            "a request that changes something must be the Holosmith client",
        )
    }

    /// What a request from a tailnet user who is not the owner answers.
    pub fn bad_owner() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            BAD_OWNER,
            "this tailnet user is not the configured owner",
        )
    }

    /// What a create whose body is not the expected JSON object answers.
    pub fn bad_body() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_BODY,
            "the body must be a JSON object with `project`, `model`, `thinking` and optionally \
             `network`",
        )
    }

    /// What a create naming a field the endpoint does not accept answers.
    pub fn unknown_field() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            UNKNOWN_FIELD,
            "the body carries a field this endpoint does not accept",
        )
    }

    /// What a create without an `Idempotency-Key` header answers.
    pub fn missing_idempotency_key() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            MISSING_IDEMPOTENCY_KEY,
            "a create must carry an `Idempotency-Key` header",
        )
    }

    /// What a create whose `Idempotency-Key` is not one bounded value answers.
    pub fn bad_idempotency_key() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_IDEMPOTENCY_KEY,
            "the `Idempotency-Key` header must be one value of at most 200 characters",
        )
    }

    /// What a create whose `project` is not one path segment answers.
    pub fn bad_project() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_PROJECT,
            "`project` must be one path segment of letters, digits, `.`, `_` and `-`, at most \
             255 characters, and not `.` or `..`",
        )
    }

    /// What a create whose `model` is not one model name answers.
    pub fn bad_model() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_MODEL,
            "`model` must be 1–128 characters of letters, digits, `.`, `:`, `/` and `-`",
        )
    }

    /// What a create whose `thinking` is not one thinking level answers.
    pub fn bad_thinking() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_THINKING,
            "`thinking` must be 1–32 lowercase letters, digits, `_` and `-`, starting with a \
             letter",
        )
    }

    /// What a create at the cap answers until HS-020 lets it evict.
    pub fn cap_reached() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            CAP_REACHED,
            "this machine already runs the maximum number of live sessions",
        )
    }

    /// What a create that failed for an unplanned reason answers.
    ///
    /// The message is one fixed sentence: a rusqlite error can carry the database path, so it
    /// goes to the log rather than to a client (ADR-0012).
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            INTERNAL,
            "the server could not complete the request",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_is_two_keys_in_order() {
        assert_eq!(
            serde_json::to_string(&ApiError::not_found().body)
                .expect("two string fields always serialise"),
            r#"{"code":"not_found","message":"no such endpoint"}"#
        );
    }

    #[test]
    fn not_found_is_a_404_with_the_specs_code() {
        let error = ApiError::not_found();
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.body.code, NOT_FOUND);
    }

    #[test]
    fn the_response_is_json() {
        let response = ApiError::not_found().into_response();
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    fn assert_refusal(error: ApiError, code: &'static str) {
        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.body.code, code);
        assert_eq!(
            error
                .into_response()
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn bad_host_is_a_403_with_its_code() {
        assert_refusal(ApiError::bad_host(), BAD_HOST);
    }

    #[test]
    fn bad_origin_is_a_403_with_its_code() {
        assert_refusal(ApiError::bad_origin(), BAD_ORIGIN);
    }

    #[test]
    fn bad_client_is_a_403_with_its_code() {
        assert_refusal(ApiError::bad_client(), BAD_CLIENT);
    }

    #[test]
    fn bad_owner_is_a_403_with_its_code() {
        assert_refusal(ApiError::bad_owner(), BAD_OWNER);
    }

    fn assert_answer(error: ApiError, status: StatusCode, code: &'static str) {
        assert_eq!(error.status, status);
        assert_eq!(error.body.code, code);
        assert_eq!(
            error
                .into_response()
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn bad_body_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_body(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_BODY,
        );
    }

    #[test]
    fn unknown_field_is_a_422_with_its_code() {
        assert_answer(
            ApiError::unknown_field(),
            StatusCode::UNPROCESSABLE_ENTITY,
            UNKNOWN_FIELD,
        );
    }

    #[test]
    fn missing_idempotency_key_is_a_422_with_its_code() {
        assert_answer(
            ApiError::missing_idempotency_key(),
            StatusCode::UNPROCESSABLE_ENTITY,
            MISSING_IDEMPOTENCY_KEY,
        );
    }

    #[test]
    fn bad_idempotency_key_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_idempotency_key(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_IDEMPOTENCY_KEY,
        );
    }

    #[test]
    fn bad_project_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_project(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_PROJECT,
        );
    }

    #[test]
    fn bad_model_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_model(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_MODEL,
        );
    }

    #[test]
    fn bad_thinking_is_a_422_with_its_code() {
        assert_answer(
            ApiError::bad_thinking(),
            StatusCode::UNPROCESSABLE_ENTITY,
            BAD_THINKING,
        );
    }

    #[test]
    fn cap_reached_is_a_409_with_its_code() {
        assert_answer(ApiError::cap_reached(), StatusCode::CONFLICT, CAP_REACHED);
    }

    #[test]
    fn internal_is_a_500_with_its_code() {
        assert_answer(
            ApiError::internal(),
            StatusCode::INTERNAL_SERVER_ERROR,
            INTERNAL,
        );
    }
}
