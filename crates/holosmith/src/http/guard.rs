//! The four browser-facing checks, as one resolved value (ADR-0010).
//!
//! [`Guards`] is built once from the config and read on every request that is not exempt, in
//! the order `bad_host` → `bad_origin` → `bad_client` → `bad_owner`. It parses nothing on the
//! request path: the allow-list is already folded to lowercase with the two implicit loopback
//! names in it, and the control origin is already folded. Whether a request is exempt is
//! [`crate::http::is_guard_exempt`]'s decision, not this module's, so the exemption list exists
//! in exactly one place.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, header, header::AsHeaderName};
use axum::response::Response;

use crate::config::Config;
use crate::http::error::ApiError;

/// The client header a Holosmith request that changes something must carry.
const CLIENT_HEADER: &str = "x-holosmith-client";

/// The header `tailscale serve` adds for the tailnet user behind a request.
const LOGIN_HEADER: &str = "tailscale-user-login";

/// The names a machine answers on even with an empty `allowed_hosts`, because the default build
/// must serve its own app shell out of the box. `[::1]` is deliberately not one of them: it is
/// a different address family, and a machine that wants it lists it.
const IMPLICIT_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

/// The value `X-Holosmith-Client` must carry, exactly.
const CLIENT_VALUE: &str = "1";

/// The only media type a request that changes something may carry.
const JSON: &str = "application/json";

/// The prefix every header the guard refuses to send a browser starts with.
const CORS_PREFIX: &str = "access-control-";

/// The four checks, resolved once per process.
#[derive(Debug, Clone)]
pub struct Guards {
    /// Lowercase, with [`IMPLICIT_HOSTS`] added: the `Host` values this machine answers.
    hosts: Vec<String>,
    /// Lowercase: the origin a request that changes something must carry.
    control_origin: String,
    /// The tailnet login the requests must come from, when one is configured.
    owner: Option<String>,
}

impl Guards {
    /// Resolve the checks from the config in force. The config's own validators already
    /// guarantee an allow-list entry is a bare ASCII name or a bracketed IPv6 literal.
    pub fn from_config(config: &Config) -> Self {
        let mut hosts: Vec<String> = config
            .allowed_hosts
            .iter()
            .map(|host| host.to_ascii_lowercase())
            .collect();
        for implicit in IMPLICIT_HOSTS {
            if !hosts.iter().any(|host| host == implicit) {
                hosts.push(implicit.to_string());
            }
        }
        Self {
            hosts,
            control_origin: config.control_origin.as_str().to_ascii_lowercase(),
            owner: config.owner.clone(),
        }
    }

    /// The first check this request fails, or `None` when it passes all four.
    ///
    /// The caller has already decided the request is not exempt. Only the method and the
    /// headers are read; the body never is.
    pub fn check(&self, request: &Request) -> Option<ApiError> {
        let headers = request.headers();
        let host = single(headers, header::HOST).map(host_of);
        if !host.is_some_and(|host| self.hosts.contains(&host)) {
            return Some(ApiError::bad_host());
        }
        if request.method() != Method::GET {
            let origin = single(headers, header::ORIGIN)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|origin| origin.trim().eq_ignore_ascii_case(&self.control_origin));
            if !origin {
                return Some(ApiError::bad_origin());
            }
            let client = single(headers, header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| media_type(value).eq_ignore_ascii_case(JSON))
                && single(headers, CLIENT_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.trim() == CLIENT_VALUE);
            if !client {
                return Some(ApiError::bad_client());
            }
        }
        if let Some(owner) = &self.owner {
            let mut logins = headers.get_all(LOGIN_HEADER).iter();
            let login = match (logins.next(), logins.next()) {
                // Direct access, not behind `tailscale serve`: there is no login to check.
                (None, _) => true,
                (Some(value), None) => value
                    .to_str()
                    .is_ok_and(|login| login.trim() == owner.as_str()),
                // Two values for one header have no defensible winner, so pick neither.
                (Some(_), Some(_)) => false,
            };
            if !login {
                return Some(ApiError::bad_owner());
            }
        }
        None
    }
}

/// Remove every `access-control-*` header, so no answer a browser receives carries one.
///
/// Not a method: it reads no [`Guards`] state, and it runs on a refusal and a route's response
/// alike (ADR-0010). `http` 1.5.0's `HeaderMap` has no `retain`, so the names are collected
/// first and then removed one by one.
pub fn strip_cors(response: &mut Response) {
    let names: Vec<_> = response
        .headers()
        .keys()
        .filter(|name| name.as_str().starts_with(CORS_PREFIX))
        .cloned()
        .collect();
    for name in names {
        response.headers_mut().remove(&name);
    }
}

/// The one value of a header that was sent at most once. Absent and repeated both answer
/// `None`: two values for one header have no defensible winner.
fn single<K>(headers: &HeaderMap, name: K) -> Option<&HeaderValue>
where
    K: AsHeaderName,
{
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (Some(value), None) => Some(value),
        _ => None,
    }
}

/// A `Host` header as the allow-list spells it: no port, a bracketed IPv6 literal kept intact,
/// ASCII-lowercased. A value no header can carry folds to something no list holds.
fn host_of(value: &HeaderValue) -> String {
    let text = String::from_utf8_lossy(value.as_bytes());
    let text = text.trim();
    let host = match text.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((literal, _port)) => format!("[{literal}]"),
            None => text.to_string(),
        },
        None => match text.split_once(':') {
            Some((host, _port)) => host.to_string(),
            None => text.to_string(),
        },
    };
    host.to_ascii_lowercase()
}

/// A media type without its parameters, trimmed. Compared ASCII-case-insensitively by the
/// caller: `application/json+something` is a different type and must not pass.
fn media_type(value: &str) -> &str {
    match value.split_once(';') {
        Some((media_type, _parameters)) => media_type.trim(),
        None => value.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> HeaderValue {
        HeaderValue::from_str(text).expect("a test header value is valid")
    }

    #[test]
    fn a_host_drops_its_port() {
        assert_eq!(host_of(&value("127.0.0.1:8080")), "127.0.0.1");
    }

    #[test]
    fn a_bracketed_ipv6_host_keeps_its_brackets() {
        assert_eq!(host_of(&value("[::1]:8080")), "[::1]");
        assert_eq!(host_of(&value("[::1]")), "[::1]");
    }

    #[test]
    fn a_host_is_folded_to_ascii_lowercase() {
        assert_eq!(host_of(&value("CONTROL.Example:8443")), "control.example");
    }

    #[test]
    fn a_host_without_a_port_is_unchanged() {
        assert_eq!(host_of(&value("localhost")), "localhost");
    }

    #[test]
    fn a_media_type_drops_its_parameters() {
        assert_eq!(media_type("application/json; charset=utf-8"), JSON);
    }

    #[test]
    fn a_media_type_without_parameters_is_itself() {
        assert_eq!(media_type("text/plain"), "text/plain");
    }

    #[test]
    fn a_json_suffix_is_a_different_media_type() {
        assert_eq!(
            media_type("application/json+something"),
            "application/json+something"
        );
        assert!(!media_type("application/json+something").eq_ignore_ascii_case(JSON));
    }

    #[test]
    fn a_single_header_is_the_one_value() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, value("a"));
        assert_eq!(
            single(&headers, header::HOST).and_then(|value| value.to_str().ok()),
            Some("a")
        );
    }

    #[test]
    fn an_absent_header_is_none() {
        let headers = HeaderMap::new();
        assert!(single(&headers, header::HOST).is_none());
    }

    #[test]
    fn a_repeated_header_is_none() {
        let mut headers = HeaderMap::new();
        headers.append(header::HOST, value("a"));
        headers.append(header::HOST, value("b"));
        assert!(
            single(&headers, header::HOST).is_none(),
            "two values for one header must not resolve to either"
        );
    }

    #[test]
    fn the_default_config_guards_two_loopback_hosts_and_no_owner() {
        let guards = Guards::from_config(&Config::default());
        assert_eq!(guards.hosts, vec!["localhost", "127.0.0.1"]);
        assert_eq!(guards.control_origin, "http://localhost:8080");
        assert_eq!(guards.owner, None);
    }
}
