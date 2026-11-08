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
use axum::extract::{Path, RawQuery, Request};
use axum::handler::Handler;
use axum::http::{HeaderMap, Method, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};

use crate::backend::Backends;
use crate::compactor::Compactor;
use crate::config::Config;
use crate::db::Database;
use crate::deleter::Deleter;
use crate::drain::{Control, Drain};
use crate::events::{EventWriter, StreamRegistry};
use crate::http::changes;
use crate::http::error::ApiError;
use crate::http::guard::{self, Guards};
use crate::http::health::{self, Health};
use crate::http::history;
use crate::http::merge;
use crate::http::publish;
use crate::http::sessions;
use crate::http::streams;
use crate::http::tool_image;
use crate::http::tool_output;
use crate::ladder::Ladder;
use crate::merge::Merges;
use crate::provisioner::Provisioner;
use crate::reconcile::Reconciler;
use crate::runtime::Runtime;
use crate::sessions::state::Transitioner;
use crate::stop::Stopper;
use crate::sweeper::Sweeper;
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
    /// Whether this process is draining for a restart (HS-051). Not the database's: the state is the
    /// process's, and a row would outlive the restart the drain exists to precede and lie about it. Every
    /// clone of this state carries the same bit, which is what lets a handler read it on the request path
    /// while the socket's own task sets it.
    pub drain: Control,
    /// One merge per project at a time (HS-054, spec item 12's rule 2). Here rather than in a handler for the
    /// reason the state's other handles are: two requests must share the lock, so it must be the state's, and a
    /// clone of the state clones the `Arc` rather than the map.
    pub merges: Merges,
    /// The external programs this *request path* runs, as opposed to the ones a startup check runs.
    ///
    /// Only `df` today, for HS-062's floor: a create and a resume measure the data root's free space before
    /// they start anything, with the same reader the boot and `doctor` use ([`crate::disk`]). The field is the
    /// state's for the reason [`Tools`](crate::preflight::Tools) is a parameter at all — this crate denies
    /// `unsafe`, so a test cannot `set_var` a `PATH`, and a shim it wrote is the only way to put a machine's
    /// answer under a test's control. The production value is the program's name, which `PATH` resolves.
    pub tools: crate::preflight::Tools,
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
    /// handles a background turn needs in one place. Since HS-022 it also carries the two durations a
    /// turn's ending needs — the ladder's `abort_grace` and `turn_timeout_min` — and since HS-023 the
    /// ceiling, so a runner cannot be built with a bound, or with a budget, the config did not ask for.
    pub fn turner(&self) -> Turn {
        Turn::new(
            self.backend.clone(),
            Arc::clone(&self.database),
            self.events.clone(),
            self.config.abort_grace.into(),
            crate::turn::timeout(self.config.turn_timeout_min),
            self.config.max_session_tokens,
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

    /// The delete flow's removal task, wired to this state's backend, database and stream registry
    /// (HS-019).
    ///
    /// Here for the same reason as the three above: the delete handler clones the three handles a
    /// background removal needs in one place. The registry is the state's own, so a session's open
    /// streams are the ones it closes.
    pub fn deleter(&self) -> Deleter {
        Deleter::new(
            self.backend.clone(),
            Arc::clone(&self.database),
            self.streams.clone(),
            self.config.secrets_path(),
        )
    }

    /// The one way a session's container is stopped, wired to this state's backend and database
    /// (HS-030).
    ///
    /// Its three users — the sweeper, the create's eviction and the abort ladder — all need the same two
    /// handles, and spec item 6's flag has to be written before the call in all three (ADR-0038). A
    /// helper here is what keeps that order in one place.
    pub fn stopper(&self) -> Stopper {
        Stopper::new(self.backend.clone(), Arc::clone(&self.database))
    }

    /// The idle stop's sweeper, wired to this state's backend, database and event writer (HS-021).
    ///
    /// `None` when `idle_stop_min` is 0, which is the key's off switch: an operator who wants no idle
    /// stop should not have to pay for a task that sweeps and finds nothing. The decision lives here
    /// rather than in `serve` so there is one place that reads the key, and it is the same shape as
    /// HS-022's turn timeout (ADR-0025).
    ///
    /// Here for the same reason as the four above: the handles a background policy needs are cloned in
    /// one place.
    pub fn sweeper(&self) -> Option<Sweeper> {
        let idle_min = self.config.idle_stop_min;
        if idle_min == 0 {
            return None;
        }
        Some(Sweeper::new(
            self.stopper(),
            Arc::clone(&self.database),
            self.events.clone(),
            // `clock::minutes`, not `* 60`: the key is a `u64` the config layer accepts without a
            // ceiling, and a wrapped bound would turn "stop after a week" into "stop immediately".
            crate::clock::minutes(idle_min),
        ))
    }

    /// The compaction task, wired to this state's database (HS-060).
    ///
    /// Always present, unlike [`Self::sweeper`]: the window is the spec's 24 hours rather than a config
    /// key, so there is no key that could turn it off (ADR-0032). It needs neither the backend nor the event
    /// writer — compaction folds rows and emits nothing.
    ///
    /// Here for the same reason as the five above: a background policy's handles are cloned in one place,
    /// so a test can drive the wiring `serve` uses rather than a second one.
    pub fn compactor(&self) -> Compactor {
        Compactor::new(Arc::clone(&self.database))
    }

    /// The per-session disk soft cap's watchdog, wired to this state's ladder, stopper, rows, writer, the
    /// `du` this run measures with and the cap itself (HS-063, spec item 14).
    ///
    /// `None` when `session_disk_soft_gb` is 0, which is the key's off switch: an operator who wants no cap
    /// should not pay for a task that measures nothing (the sweeper's rule, ADR-0025). The decision lives here
    /// rather than in `serve` so there is one place that reads the key — and so a `0` never reaches
    /// [`crate::quota::over`], whose comparison against a zero cap would answer "over" for any worktree at all.
    ///
    /// Here for the same reason as the six above: the handles a background policy needs are cloned in one
    /// place, and the ladder is the state's own, so the `abort_grace` a forced stop waits out is the config's.
    pub fn quota(&self) -> Option<crate::quota::Quota> {
        let limit_gb = self.config.session_disk_soft_gb;
        if limit_gb == 0 {
            return None;
        }
        Some(crate::quota::Quota::new(
            self.ladder(),
            self.stopper(),
            Arc::clone(&self.database),
            self.events.clone(),
            self.tools.du.clone(),
            limit_gb,
        ))
    }

    /// The stopped-session expiry's daily job, wired to this state (HS-064, spec item 15).
    ///
    /// **Always present, unlike [`Self::sweeper`] and [`Self::quota`]** — there is no off switch, and the absence
    /// is a decision rather than an omission: `stopped_ttl_days` already refuses `0` (`config/mod.rs`), and a `0`
    /// here would mean "remove every stopped session the moment it stops", not "never". An operator who wants no
    /// expiry raises the number. A `None` arm would be a second spelling of an answer the config already
    /// gives.
    ///
    /// It takes the state itself rather than four handles, because it calls the delete flow's own second half
    /// ([`crate::http::sessions::complete_deletion`]) and a second wiring of backend, transitioner, deleter and
    /// `[merge] targets` would be exactly the drift that rule exists to prevent.
    ///
    /// Here for the same reason as the eight above: the handles a background policy needs are cloned in one
    /// place, so a test drives the wiring `serve` uses rather than a second one.
    pub fn expiry(&self) -> crate::expiry::Expiry {
        crate::expiry::Expiry::new(self.clone(), self.config.stopped_ttl_days)
    }

    /// The startup reconcile, wired to this state's rows, backend, writer, config and this run's backend
    /// (HS-044).
    ///
    /// Here for the same reason as the six above, and with one extra job: the *runtime* backend decides
    /// whether the name half runs at all (containers and networks exist only in a Docker run), so the two
    /// words `serve` resolved — the config's and this run's — cannot be mixed up by a caller.
    pub fn reconciler(&self) -> Reconciler {
        Reconciler::new(
            Arc::clone(&self.database),
            self.events.clone(),
            self.backend.clone(),
            &self.config,
            self.runtime.backend,
        )
    }

    /// The drain's sequence, wired to this state's flag, rows, stopper and event writer (HS-051).
    ///
    /// Here for the same reason as the seven above, and with one extra job: the flag is *this* state's, so a
    /// drain built anywhere else would set a bit no handler reads. `None` of the four handles could be wired
    /// wrong without the type saying so, which is the whole argument for the helpers.
    pub fn drainer(&self) -> Drain {
        Drain::new(
            self.drain.clone(),
            Arc::clone(&self.database),
            self.stopper(),
            self.events.clone(),
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
    let history_state = Arc::clone(&state);
    let tool_output_state = Arc::clone(&state);
    let tool_image_state = Arc::clone(&state);
    let changes_state = Arc::clone(&state);
    let changes_diff_state = Arc::clone(&state);
    let merge_state = Arc::clone(&state);
    let publish_state = Arc::clone(&state);
    let prompt_state = Arc::clone(&state);
    let abort_state = Arc::clone(&state);
    let steer_state = Arc::clone(&state);
    let resume_state = Arc::clone(&state);
    let delete_state = Arc::clone(&state);
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
        // One older page of the same log (HS-061). Read-only like its neighbours, so the manifest records
        // it `ReadOnly` and the guards apply with nothing new. The query is taken as `RawQuery`, whose
        // rejection is `Infallible`: `before` is required and `limit` is clamped, and both are the
        // handler's to read and to refuse with `bad_query`.
        //
        // The path is a `Result` for the same reason as its siblings — a `%FF` segment is a session the
        // table does not hold — and a second route on the same prefix that answered `text/plain` would be
        // one rule with two spellings.
        .get(
            "/api/sessions/{id}/events/history",
            move |id: Result<Path<String>, PathRejection>, query: RawQuery| {
                let state = Arc::clone(&history_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { history::respond(&state, &id, query).await }
            },
        )
        // One tool call's full output, as a file (HS-033). Read-only like its neighbours, so the manifest
        // records `ReadOnly` and every guard applies with nothing new. Both path segments are taken as
        // `Result`s: a `%FF` segment is not UTF-8, and an id that is not a name is a file that cannot exist —
        // the handler answers `not_found` for each, which is one answer to one question (a traversal is
        // impossible because the id is checked against an allow-list before anything is joined).
        .get(
            "/api/sessions/{id}/tool-output/{tool_id}",
            move |captured: Result<Path<(String, String)>, PathRejection>| {
                let state = Arc::clone(&tool_output_state);
                let (id, tool_id) = captured.map(|Path(pair)| pair).unwrap_or_default();
                async move { tool_output::respond(&state, &id, &tool_id).await }
            },
        )
        // One image a tool returned, as a file beside that output (HS-035). Read-only on the same terms as the
        // route above, and the same three `Result`s: the index is a number the handler parses, so a segment that
        // is not one is `not_found` rather than a second kind of error.
        .get(
            "/api/sessions/{id}/tool-image/{tool_id}/{index}",
            move |captured: Result<Path<(String, String, String)>, PathRejection>| {
                let state = Arc::clone(&tool_image_state);
                let (id, tool_id, index) =
                    captured.map(|Path(captured)| captured).unwrap_or_default();
                async move { tool_image::respond(&state, &id, &tool_id, &index).await }
            },
        )
        // The changes view's summary (HS-052): what a session changed and whether it can merge. Read-only on the
        // same terms as its neighbours — the manifest records it `ReadOnly` and every guard applies — and it is
        // the one session route whose work is host git in the project repository rather than the database or the
        // backend. The path is a `Result` for the same reason as its siblings: a `%FF` segment is a session the
        // table does not hold.
        .get(
            "/api/sessions/{id}/changes",
            move |id: Result<Path<String>, PathRejection>| {
                let state = Arc::clone(&changes_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { changes::respond(&state, &id).await }
            },
        )
        // The unified diff (HS-053): spec item 12's second bullet, `…/changes`'s read with a `path` to narrow it.
        // Read-only on the same terms as its neighbours — a `GET`, so the manifest records `ReadOnly` and every
        // guard applies — and host git in the project repository like the summary beside it. The query is taken
        // as `RawQuery`, whose rejection is `Infallible`: `path` is optional (no `path` is the whole branch) and
        // what it means is the handler's to read, refuse and answer. The path is a `Result` for the same reason
        // as its siblings: a `%FF` segment is a session the table does not hold.
        .get(
            "/api/sessions/{id}/changes/diff",
            move |id: Result<Path<String>, PathRejection>, query: RawQuery| {
                let state = Arc::clone(&changes_diff_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { changes::respond_diff(&state, &id, query).await }
            },
        )
        // The merge (HS-054): spec item 12's rules 2 and 4, and the first route in this build whose work writes
        // to the project repository. Mutating, so every guard applies and the manifest records it; `routes.post`
        // is what makes a `ReadOnly` kind unexpressible. Host git like the changes view beside it — no container
        // is asked anything — and the one route here that takes a lock the state owns, because two merges into
        // one project must not interleave. The path and the body are `Result`s for the reason the prompt's are.
        .post(
            "/api/sessions/{id}/merge",
            move |id: Result<Path<String>, PathRejection>, body: Result<Bytes, BytesRejection>| {
                let state = Arc::clone(&merge_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { merge::respond(&state, &id, body).await }
            },
        )
        // The publish (HS-057): item 12's last route, and the first in this build whose work reaches **out** of
        // the machine. Mutating, so every guard applies and a `ReadOnly` kind is unexpressible; host git like the
        // merge beside it — the push is the server user's own git, and no container is asked anything — and the
        // path and the body are `Result`s for the reason the prompt's are. The manifest's kind is what keeps a
        // route that writes somebody else's repository behind ADR-0010's three headers.
        .post(
            "/api/sessions/{id}/publish",
            move |id: Result<Path<String>, PathRejection>, body: Result<Bytes, BytesRejection>| {
                let state = Arc::clone(&publish_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { publish::respond(&state, &id, body).await }
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
        // The delete (HS-019): the one route in this table whose method is not `GET` or `POST`, and
        // `Routes::delete` already records it `Mutating`, so the guards apply as they do to a
        // prompt. The query is taken as `RawQuery`, whose rejection is `Infallible`: `?force=true`
        // is the only thing a delete's query can say, and it is the handler's to read.
        .delete(
            "/api/sessions/{id}",
            move |id: Result<Path<String>, PathRejection>, query: RawQuery| {
                let state = Arc::clone(&delete_state);
                let id = id.map(|Path(id)| id).unwrap_or_default();
                async move { sessions::delete(&state, &id, query).await }
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
