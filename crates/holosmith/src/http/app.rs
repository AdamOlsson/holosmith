//! Routes and the one servable app: the seam HS-005 attaches its guards to (ADR-0007).
//!
//! Feature code adds routes to [`Routes`]. The only value that can be served is [`App`], and
//! `Router::layer` runs in exactly one place — [`App::new`] — because a layer applied at the
//! call site would wrap only the routes that existed when it ran, and a route added later
//! would silently skip it. That single layer is also what makes the guards unavoidable: a
//! route registered by any later story passes through them without knowing they exist.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{Path, Request};
use axum::handler::Handler;
use axum::http::{HeaderMap, Method, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};

use crate::backend::Backends;
use crate::config::Config;
use crate::db::Database;
use crate::events::{EventWriter, StreamRegistry};
use crate::http::error::ApiError;
use crate::http::guard::{self, Guards};
use crate::http::health::{self, Health};
use crate::http::sessions;
use crate::http::streams;
use crate::ladder::Ladder;
use crate::provisioner::Provisioner;
use crate::runtime::Runtime;
use crate::sessions::state::Transitioner;
use crate::turn::Turn;
use crate::web;

/// What a handler may read: the config in force, this run's backend and start time, the one open
/// database, the event writer's handle, the sessions with a stream open, and the backend that
/// performs provisioning and the lifecycle.
///
/// The database is reached through the `Arc<Mutex<Database>>` ADR-0012 introduced; ADR-0013 keeps
/// it and adds the writer thread on top: the guard is held only across synchronous repository
/// calls and never across an `.await`, and the writer takes it for its own transaction only. The
/// writer handle is a sender, so cloning the state clones a queue, not the writer. The stream
/// registry is one `Arc`, cloned with the state per request (ADR-0014). `backend` is the value a
/// spawned provisioner takes with it; `runtime.backend` is only which one this run chose
/// (ADR-0015).
#[derive(Debug, Clone)]
pub struct AppState {
    pub config: Config,
    pub runtime: Runtime,
    pub database: Arc<Mutex<Database>>,
    pub events: EventWriter,
    /// Which sessions have an SSE stream open: HS-019 closes them, HS-066 reads the count.
    pub streams: StreamRegistry,
    pub backend: Backends,
}

impl AppState {
    /// A provisioner wired to this state's backend, database and event writer.
    ///
    /// Here rather than in the handler so the three handles a background task needs are cloned in
    /// one place: a second wiring would be a second thing to keep right.
    pub fn provisioner(&self) -> Provisioner {
        Provisioner::new(
            self.backend.clone(),
            Arc::clone(&self.database),
            self.events.clone(),
        )
    }

    /// A turn runner wired to this state's backend, database and event writer (HS-015).
    ///
    /// Here for the same reason as [`Self::provisioner`]: the prompt handler clones the three
    /// handles a background turn needs in one place.
    pub fn turner(&self) -> Turn {
        Turn::new(
            self.backend.clone(),
            Arc::clone(&self.database),
            self.events.clone(),
        )
    }

    /// The one state function (HS-012), wired to this state's database and event writer.
    ///
    /// Here for the same reason as [`Self::provisioner`]: a story that changes a session's state
    /// reaches it through the state it already has, so the two handles cannot be wired wrong or
    /// half-wired.
    pub fn transitioner(&self) -> Transitioner {
        Transitioner::new(Arc::clone(&self.database), self.events.clone())
    }

    /// An abort ladder wired to this state's backend, database, event writer and `abort_grace`
    /// (HS-016).
    ///
    /// Here for the same reason as [`Self::turner`]: the abort handler clones the handles a
    /// background ladder needs in one place, and the grace period is the config's rather than a
    /// second number a caller could choose.
    pub fn ladder(&self) -> Ladder {
        Ladder::new(
            self.backend.clone(),
            Arc::clone(&self.database),
            self.events.clone(),
            self.config.abort_grace.into(),
        )
    }
}

/// Whether a route may change state. Recorded at registration, because item 4's "`GET`
/// endpoints are read-only" is not something a test can observe from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    ReadOnly,
    Mutating,
}

/// A registered route, as the app holds it for inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub path: String,
    pub method: Method,
    pub kind: RouteKind,
}

/// A route table. There is no constructor from a bare `Router` and no accessor that returns
/// one, so a route can only exist inside the app that carries the layer.
pub struct Routes {
    router: Router<()>,
    manifest: Vec<ManifestEntry>,
}

impl Routes {
    /// An empty table. Feature code builds one and hands it to [`App::new`].
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            manifest: Vec::new(),
        }
    }

    /// A read-only route answered by `GET`.
    pub fn get<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.register(path, Method::GET, RouteKind::ReadOnly, get(handler))
    }

    /// A read-only route answered by `HEAD`.
    pub fn head<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.register(
            path,
            Method::HEAD,
            RouteKind::ReadOnly,
            axum::routing::head(handler),
        )
    }

    /// A mutating route answered by `POST`.
    pub fn post<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.register(
            path,
            Method::POST,
            RouteKind::Mutating,
            axum::routing::post(handler),
        )
    }

    /// A mutating route answered by `PUT`.
    pub fn put<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.register(
            path,
            Method::PUT,
            RouteKind::Mutating,
            axum::routing::put(handler),
        )
    }

    /// A mutating route answered by `PATCH`.
    pub fn patch<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.register(
            path,
            Method::PATCH,
            RouteKind::Mutating,
            axum::routing::patch(handler),
        )
    }

    /// A mutating route answered by `DELETE`.
    pub fn delete<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.register(
            path,
            Method::DELETE,
            RouteKind::Mutating,
            axum::routing::delete(handler),
        )
    }

    /// Set the handler for every request no route matches.
    ///
    /// The manifest records nothing for it: it answers every method, and its body only reads
    /// (an asset, the app shell, the not-found envelope), so a mutating fallback is not
    /// expressible through this API.
    pub fn fallback<H, T>(self, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        Self {
            router: self.router.fallback(handler),
            manifest: self.manifest,
        }
    }

    /// Add one method to one path and record what it is. Each helper above takes a handler
    /// rather than a `MethodRouter`, which is what makes a `Mutating` kind on a `GET`
    /// unexpressible; axum merges the method routers of a path, so later methods merge too.
    /// The kind is what each helper declares, never what the method implies: deriving it would
    /// make the manifest test a tautology (ADR-0010).
    fn register(
        self,
        path: &str,
        method: Method,
        kind: RouteKind,
        methods: MethodRouter<()>,
    ) -> Self {
        let Self {
            router,
            mut manifest,
        } = self;
        manifest.push(ManifestEntry {
            path: path.to_string(),
            method,
            kind,
        });
        Self {
            router: router.route(path, methods),
            manifest,
        }
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
    manifest: Vec<ManifestEntry>,
}

impl App {
    /// Wrap the route table in the layer every request passes through, fallback included.
    ///
    /// The layer is the one `Router::layer` call in the crate (ADR-0007), so the guards and the
    /// counter cannot be assembled in a different order anywhere else.
    pub fn new(routes: Routes, guards: Guards) -> Self {
        let Routes { router, manifest } = routes;
        let requests = RequestCounter::new();
        let counter = Arc::clone(&requests.0);
        // Cloned per request, so the guards are shared rather than rebuilt: the allow-list is
        // resolved once, at startup.
        let guards = Arc::new(guards);
        Self {
            router: router.layer(middleware::from_fn(move |request, next| {
                let counter = Arc::clone(&counter);
                let guards = Arc::clone(&guards);
                async move { edge(counter, guards, request, next).await }
            })),
            requests,
            manifest,
        }
    }

    /// This app's request counter, so a test can prove the layer ran on a given path. Hidden
    /// from the docs on purpose: observation is not part of the seam HS-005 extends.
    #[doc(hidden)]
    pub fn counter(&self) -> RequestCounter {
        self.requests.clone()
    }

    /// The routes this app carries and whether each may change state. Hidden from the docs for
    /// the same reason as [`Self::counter`]: evidence for a test, not part of the seam.
    #[doc(hidden)]
    pub fn manifest(&self) -> &[ManifestEntry] {
        &self.manifest
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

/// The control listener's route table: the health endpoint, the sessions collection (its list, its
/// create and one row), the session's event stream, a session's prompt, abort, steer and resume, the
/// embedded app, and the two answers to an unknown path (ADR-0007).
pub fn router(state: AppState) -> Routes {
    let state = Arc::new(state);
    let health_state = Arc::clone(&state);
    let list_state = Arc::clone(&state);
    let one_state = Arc::clone(&state);
    let create_state = Arc::clone(&state);
    let stream_state = Arc::clone(&state);
    let prompt_state = Arc::clone(&state);
    let abort_state = Arc::clone(&state);
    let steer_state = Arc::clone(&state);
    let resume_state = Arc::clone(&state);
    Routes::new()
        .get("/api/health", move || {
            let state = Arc::clone(&health_state);
            async move { health::respond(&Health::of(&state.config, &state.runtime)) }
        })
        // Read-only, so the guards apply as they do to every other `GET`, the manifest records
        // `ReadOnly`, and nothing new is needed for HS-013's list (ADR-0017).
        .get("/api/sessions", move || {
            let state = Arc::clone(&list_state);
            async move { sessions::list(&state).await }
        })
        // One row, for the session screen's first answer (HS-014). Read-only like the list, and
        // `not_found` for an id the table does not hold.
        //
        // The path is taken as a `Result`, for the same reason the create takes its body as one:
        // a `%FF` segment is not valid UTF-8, and a rejection left to axum answers `text/plain`
        // 400 outside ADR-0008's envelope. A segment that is not a name we could look up is a
        // name the table does not hold, so the answer is the envelope's `not_found`.
        .get(
            "/api/sessions/{id}",
            move |id: Result<Path<String>, PathRejection>| {
                let state = Arc::clone(&one_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { sessions::one(&state, &id).await }
            },
        )
        // The body is taken as a `Result`, not as `Bytes`: a rejection left to axum answers a
        // `text/plain` 400/413 outside ADR-0008's envelope, so the handler maps it to `bad_body`
        // itself (ADR-0012).
        .post(
            "/api/sessions",
            move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                let state = Arc::clone(&create_state);
                async move { sessions::create(&state, headers, body).await }
            },
        )
        // Read-only, so the guards apply as they do to every other `GET` and the manifest says
        // which kind it is (D7). `{id}` is axum 0.8's capture syntax; the spec writes `:id`.
        //
        // The path is a `Result` here too: HS-014's route answers the envelope for a segment that
        // is not UTF-8, and a second route on the same prefix that answered `text/plain` would be
        // one rule with two spellings.
        .get(
            "/api/sessions/{id}/events",
            move |id: Result<Path<String>, PathRejection>, headers: HeaderMap| {
                let state = Arc::clone(&stream_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { streams::respond(&state, &id, &headers).await }
            },
        )
        // The prompt: a mutating route, so every guard applies and the manifest records it (HS-015).
        // The path is a `Result` for the same reason as the routes above — a `%FF` segment is a
        // session the table does not hold — and the body is one, so a rejection axum would answer
        // `text/plain` becomes ADR-0008's `bad_body`.
        .post(
            "/api/sessions/{id}/prompt",
            move |id: Result<Path<String>, PathRejection>, body: Result<Bytes, BytesRejection>| {
                let state = Arc::clone(&prompt_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { sessions::prompt(&state, &id, body).await }
            },
        )
        // The abort: a mutating route on the same terms as the prompt (HS-016). No body extractor:
        // there is nothing to parse, so there is no rejection to map.
        .post(
            "/api/sessions/{id}/abort",
            move |id: Result<Path<String>, PathRejection>| {
                let state = Arc::clone(&abort_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { sessions::abort(&state, &id).await }
            },
        )
        // The steer: a mutating route with a body, so it takes both extractors as `Result`s for the
        // reason the prompt does (HS-017).
        .post(
            "/api/sessions/{id}/steer",
            move |id: Result<Path<String>, PathRejection>, body: Result<Bytes, BytesRejection>| {
                let state = Arc::clone(&steer_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { sessions::steer(&state, &id, body).await }
            },
        )
        // The resume: a mutating route with no body (HS-018). The path is a `Result` for the same
        // reason as its siblings — a `%FF` segment is a session the table does not hold.
        .post(
            "/api/sessions/{id}/resume",
            move |id: Result<Path<String>, PathRejection>| {
                let state = Arc::clone(&resume_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { sessions::resume(&state, &id).await }
            },
        )
        .fallback(fallback)
}

/// The `edge` layer: request accounting, the guards, and one `debug` line, in that order.
///
/// A rejected request is counted like any other, which is why the counter runs first. The CORS
/// strip runs on both answers — a refusal and a route's response — because "no CORS header,
/// ever" is unconditional (ADR-0010).
async fn edge(
    counter: Arc<AtomicU64>,
    guards: Arc<Guards>,
    request: Request,
    next: Next,
) -> Response {
    let served = counter.fetch_add(1, Ordering::Relaxed) + 1;
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let mut response = if is_guard_exempt(&method, &path) {
        next.run(request).await
    } else {
        match guards.check(&request) {
            Some(error) => error.into_response(),
            None => next.run(request).await,
        }
    };
    guard::strip_cors(&mut response);
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
            .get("/api/health", || async { "ok" })
            .fallback(|| async { "fallback" });
        let app = App::new(routes, Guards::from_config(&Config::default()));
        assert_eq!(app.counter().served(), 0);
    }
}
