//! Routes and the one servable app: the seam HS-005 attaches its guards to (ADR-0007).
//!
//! Feature code adds routes to [`Routes`]. The only value that can be served is [`App`], and
//! `Router::layer` runs in exactly one place — [`App::new`] — because a layer applied at the
//! call site would wrap only the routes that existed when it ran, and a route added later
//! would silently skip it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::extract::Request;
use axum::http::{Method, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};

use crate::config::Config;
use crate::http::error::ApiError;
use crate::http::health::{self, Health};
use crate::runtime::Runtime;
use crate::web;

/// What a handler may read: the config in force and this run's backend and start time.
#[derive(Debug, Clone)]
pub struct AppState {
    pub config: Config,
    pub runtime: Runtime,
}

/// A route table. There is no constructor from a bare `Router` and no accessor that returns
/// one, so a route can only exist inside the app that carries the layer.
pub struct Routes(Router<()>);

impl Routes {
    /// An empty table. Feature code builds one and hands it to [`App::new`].
    pub fn new() -> Self {
        Self(Router::new())
    }

    /// Add a route. The handler takes no arguments beyond what axum extracts.
    pub fn route(self, path: &str, methods: MethodRouter<()>) -> Self {
        Self(self.0.route(path, methods))
    }

    /// Set the handler for every request no route matches.
    pub fn fallback<H, T>(self, handler: H) -> Self
    where
        H: axum::handler::Handler<T, ()>,
        T: 'static,
    {
        Self(self.0.fallback(handler))
    }
}

impl Default for Routes {
    fn default() -> Self {
        Self::new()
    }
}

/// The only servable value.
pub struct App {
    router: Router<()>,
    requests: RequestCounter,
}

impl App {
    /// Wrap the route table in the layer every request passes through, fallback included.
    pub fn new(routes: Routes) -> Self {
        let requests = RequestCounter::new();
        let counter = Arc::clone(&requests.0);
        Self {
            router: routes.0.layer(middleware::from_fn(move |request, next| {
                let counter = Arc::clone(&counter);
                async move { edge(counter, request, next).await }
            })),
            requests,
        }
    }

    /// This app's request counter, so a test can prove the layer ran on a given path. Hidden
    /// from the docs on purpose: observation is not part of the seam HS-005 extends.
    #[doc(hidden)]
    pub fn counter(&self) -> RequestCounter {
        self.requests.clone()
    }

    /// The router `axum::serve` takes. Crate-private on purpose: [`crate::serve::run`] is the
    /// only caller, so nothing outside this crate can serve a router that skipped [`App::new`].
    pub(crate) fn into_router(self) -> Router<()> {
        self.router
    }
}

/// How many requests one [`App`] has answered.
#[derive(Debug, Clone)]
pub struct RequestCounter(Arc<AtomicU64>);

impl RequestCounter {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    /// Requests answered since the app was built.
    pub fn served(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// The control listener's route table: the health endpoint, the embedded app, and the two
/// answers to an unknown path (ADR-0007).
pub fn router(state: AppState) -> Routes {
    let state = Arc::new(state);
    Routes::new()
        .route(
            "/api/health",
            get(move || {
                let state = Arc::clone(&state);
                async move { health::respond(&Health::of(&state.config, &state.runtime)) }
            }),
        )
        .fallback(fallback)
}

/// The `edge` layer: request accounting and one `debug` line. It is the extension point, not
/// a guard — HS-005 adds the checks here, and [`is_guard_exempt`] is the list it reads.
async fn edge(counter: Arc<AtomicU64>, request: Request, next: Next) -> Response {
    let served = counter.fetch_add(1, Ordering::Relaxed) + 1;
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let response = next.run(request).await;
    tracing::debug!(
        target: "holosmith::http",
        served,
        %method,
        path = %path,
        status = response.status().as_u16(),
        "request"
    );
    response
}

/// The only request no guard applies to: the health endpoint, on `GET` or `HEAD`.
///
/// HS-005 reads this predicate instead of inventing its own list. `HEAD` is included because
/// axum answers it from the `GET` route, so exempting one and not the other would break the
/// header probe the operator and `systemd` use.
pub fn is_guard_exempt(method: &Method, path: &str) -> bool {
    path == "/api/health" && (*method == Method::GET || *method == Method::HEAD)
}

/// Everything no route matched.
///
/// A file the app ships is served as itself; anything else a browser asks for outside `/api/`
/// is the app shell, so `/sessions/7` survives a reload without the server knowing about it.
/// Under `/api/`, and for methods that are not `GET` or `HEAD`, the answer is ADR-0008's
/// envelope: no HTML error pages.
async fn fallback(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    if method != Method::GET && method != Method::HEAD {
        return ApiError::not_found().into_response();
    }
    if let Some((bytes, content_type)) = web::asset(path) {
        return ([(header::CONTENT_TYPE, content_type)], bytes).into_response();
    }
    if path.starts_with("/api/") {
        return ApiError::not_found().into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        web::index_html(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_health_endpoint_is_exempt_on_get_and_head_only() {
        assert!(is_guard_exempt(&Method::GET, "/api/health"));
        assert!(is_guard_exempt(&Method::HEAD, "/api/health"));
        for method in [Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS] {
            assert!(
                !is_guard_exempt(&method, "/api/health"),
                "{method} on the health endpoint must still be checked"
            );
        }
    }

    #[test]
    fn nothing_else_is_exempt() {
        for path in [
            "/",
            "/api/",
            "/api/health/",
            "/api/healthz",
            "/api/sessions",
            "/index.html",
        ] {
            assert!(
                !is_guard_exempt(&Method::GET, path),
                "{path} must not be exempt"
            );
        }
    }

    #[test]
    fn a_route_table_takes_routes_and_a_fallback() {
        // Only the compile is asserted here: `Routes` has no constructor from a `Router` and
        // no accessor returning one, which is the rule the type exists for. The behaviour of
        // the table and of the layer is `tests/http_contract.rs`.
        let routes = Routes::new()
            .route("/api/health", get(|| async { "ok" }))
            .fallback(|| async { "fallback" });
        let app = App::new(routes);
        assert_eq!(app.counter().served(), 0);
    }
}
