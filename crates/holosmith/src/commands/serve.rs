//! `holosmith serve`: the control listener in the foreground (ADR-0006, ADR-0007).
//!
//! The order is the contract: read the config, apply the CLI overrides, bind, re-derive the
//! control origin from the bound port when `--port 0` asked the kernel for one, print the
//! startup banner, then serve until a signal and the drain deadline. Every failure before the
//! banner exits 1 with the reason on stderr, and the process never serves half a config.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use tokio::net::TcpListener;

use crate::backend::Backends;
use crate::clock;
use crate::config::{Config, LoadError, LoadedConfig, Overrides};
use crate::db::{self, Database, OpenOptions};
use crate::events::{self, StreamRegistry, WriterOptions};
use crate::http::{self, App, AppState, Guards};
use crate::logging;
use crate::runtime::{Backend, Runtime};
use crate::serve::{self, ServeOptions};
use crate::version::VersionInfo;

/// What the command line asked for.
#[derive(Debug)]
pub struct ServeArgs {
    pub config: Option<PathBuf>,
    pub overrides: Overrides,
    /// The backend for this run; `None` means the config file's `backend` decides.
    pub backend: Option<Backend>,
}

pub fn run(args: ServeArgs) -> ExitCode {
    let filter = logging::init();
    let mut loaded = match Config::load(args.config) {
        Ok(loaded) => loaded,
        Err(LoadError::Invalid(problems)) => {
            eprint!("{problems}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("holosmith: serve: {error}");
            return ExitCode::FAILURE;
        }
    };
    loaded.config.apply_overrides(&args.overrides);
    let override_problems = loaded.config.override_problems();
    if !override_problems.is_empty() {
        for problem in &override_problems {
            eprintln!("{}: {problem}", loaded.path.display());
        }
        return ExitCode::FAILURE;
    }
    // `serve` owns the data root: ADR-0006's handoff said `serve` creates no directories, so
    // this story creates it (ADR-0009), and a database that cannot be opened or migrated is a
    // startup failure before any port exists.
    if let Err(error) = std::fs::create_dir_all(&loaded.config.data_root) {
        eprintln!(
            "holosmith: serve: cannot create the data root {}: {error}",
            loaded.config.data_root.display()
        );
        return ExitCode::FAILURE;
    }
    // Held for the process lifetime. ADR-0012's lock is kept (ADR-0013): the event writer is the
    // single writer for events and takes the lock only for its own transaction, so a handler's
    // guard is still taken for one synchronous block and never held across an `.await`.
    let database = match Database::open(
        &loaded.config.data_root.join(db::DATABASE_FILE),
        OpenOptions::default(),
        clock::unix_now(),
    ) {
        Ok(database) => database,
        Err(error) => {
            eprintln!("holosmith: serve: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The CLI wins over the file, the same rule `--port` and `--data-dir` follow: the file is the
    // machine's setting and the flag is this run's (ADR-0006, ADR-0015). It is written back into
    // the loaded value as well as used here, because an override ADR-0006 applies is applied to
    // the value every later reader sees — otherwise `config.backend` and `runtime.backend` would
    // disagree for exactly one run.
    if let Some(backend) = args.backend {
        loaded.config.backend = backend;
    }
    let backend = loaded.config.backend;
    // `--port 0` asks the kernel for a port, so the origin a browser will send is not knowable
    // until after `bind`; the flag decides, because a port written in the file may deliberately
    // name a proxied address rather than the local one.
    let ephemeral_origin = args.overrides.port == Some(0);
    let runtime = Runtime::started(backend);
    let version = VersionInfo::current();
    let listen = loaded.config.control_listen;

    let body = async move {
        // Installed before the listener binds, so a handler that cannot be installed is a
        // startup failure rather than a process no signal can stop.
        let stop = match serve::StopSignal::install() {
            Ok(stop) => stop,
            Err(error) => {
                eprintln!("holosmith: serve: cannot install the signal handler: {error}");
                return ExitCode::FAILURE;
            }
        };
        let listener = match TcpListener::bind(listen).await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("holosmith: serve: cannot listen on {listen}: {error}");
                return ExitCode::FAILURE;
            }
        };
        let bound = match listener.local_addr() {
            Ok(bound) => bound,
            Err(error) => {
                eprintln!("holosmith: serve: cannot read the bound address of {listen}: {error}");
                return ExitCode::FAILURE;
            }
        };
        if ephemeral_origin {
            loaded.config.control_origin = loaded.config.control_origin.with_port(bound.port());
        }
        banner(&filter, &loaded, &runtime, &version, bound, &database);
        let guards = Guards::from_config(&loaded.config);
        let database = Arc::new(Mutex::new(database));
        // The writer thread outlives this scope, so its handle is what the app carries. The
        // stream `spawn` also hands back is dropped: every subscriber is made from the handle
        // with `EventWriter::subscribe`, because a stream cannot be cloned into an `AppState`
        // and each one starts at the point of the broadcast (ADR-0013, ADR-0014).
        let (events, _stream) = events::spawn(Arc::clone(&database), WriterOptions::default());
        let state = AppState {
            config: loaded.config.clone(),
            runtime,
            database,
            events,
            streams: StreamRegistry::new(),
            backend: Backends::for_run(backend),
        };
        // Built before the state is handed to the router, because the sweeper is a task rather than a
        // route: `None` means `idle_stop_min = 0` turned it off. Both facts are `debug` lines and not
        // banner lines: the startup contract is the eight lines `tests/serve_cli.rs` pins byte for
        // byte, and a policy's own state is diagnostics, which `HOLOSMITH_LOG` decides whether to show.
        let sweeper = match state.sweeper() {
            Some(sweeper) => {
                tracing::debug!(
                    target: "holosmith::serve",
                    idle_min = loaded.config.idle_stop_min,
                    tick_s = crate::sweeper::TICK.as_secs(),
                    "the idle stop sweeper is running"
                );
                Some(sweeper.spawn())
            }
            None => {
                tracing::debug!(
                    target: "holosmith::serve",
                    "idle_stop_min is 0, so no session is stopped for being idle"
                );
                None
            }
        };
        let app = App::new(http::router(state), guards);
        let served = serve::run(listener, app, ServeOptions::default(), stop.recv()).await;
        // The sweeper is a policy over rows, not part of serving: when the listener's drain is over,
        // the process has no more questions to ask, so its next tick must not hold anything up. The
        // runtime drops the task anyway; aborting says so, and makes the handle's purpose its comment.
        if let Some(sweeper) = sweeper {
            sweeper.abort();
        }
        match served {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("holosmith: serve: {error}");
                ExitCode::FAILURE
            }
        }
    };

    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(body),
        Err(error) => {
            eprintln!("holosmith: serve: cannot start the async runtime: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The eight startup lines, in this order, on stderr.
///
/// They are written directly rather than logged as events so they survive any
/// `HOLOSMITH_LOG` filter: an operator reading `journalctl` sees the addresses the process
/// bound, the origin HS-005 will compare a browser's `Origin` against, the config it read, and
/// the database it opened and migrated, even when diagnostics are off.
fn banner(
    filter: &str,
    loaded: &LoadedConfig,
    runtime: &Runtime,
    version: &VersionInfo,
    bound: SocketAddr,
    database: &Database,
) {
    eprintln!("log {filter}");
    eprintln!(
        "holosmith {} target {} role {} backend {}",
        version.version, version.target, loaded.config.role, runtime.backend
    );
    eprintln!("config {}", loaded.describe());
    eprintln!("data_root {}", loaded.config.data_root.display());
    eprintln!("control_listen {bound}");
    eprintln!("control_origin {}", loaded.config.control_origin.as_str());
    eprintln!(
        "preview_listen {} (configured, not bound until HS-041)",
        loaded.config.preview_listen
    );
    eprintln!(
        "database {} schema {}",
        database.path().display(),
        database.schema_version()
    );
}
