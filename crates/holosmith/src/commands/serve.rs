//! `holosmith serve`: the control listener in the foreground (ADR-0006, ADR-0007).
//!
//! The order is the contract: read the config, apply the CLI overrides, bind, re-derive the
//! control origin from the bound port when `--port 0` asked the kernel for one, print the
//! startup banner, then serve until a signal and the drain deadline. Every failure before the
//! banner exits 1 with the reason on stderr, and the process never serves half a config.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::TcpListener;

use crate::config::{Config, LoadError, LoadedConfig, Overrides};
use crate::db::{self, Database, OpenOptions};
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
    pub backend: Backend,
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
    // Held for the process lifetime. The connection is deliberately **not** in `AppState`:
    // sharing it between handlers is the single-writer decision HS-009 owns.
    let database = match Database::open(
        &loaded.config.data_root.join(db::DATABASE_FILE),
        OpenOptions::default(),
        unix_now(),
    ) {
        Ok(database) => database,
        Err(error) => {
            eprintln!("holosmith: serve: {error}");
            return ExitCode::FAILURE;
        }
    };
    // `--port 0` asks the kernel for a port, so the origin a browser will send is not knowable
    // until after `bind`; the flag decides, because a port written in the file may deliberately
    // name a proxied address rather than the local one.
    let ephemeral_origin = args.overrides.port == Some(0);
    let runtime = Runtime::started(args.backend);
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
        let app = App::new(
            http::router(AppState {
                config: loaded.config.clone(),
                runtime,
            }),
            guards,
        );
        match serve::run(listener, app, ServeOptions::default(), stop.recv()).await {
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

/// Unix seconds recorded as `schema_version.applied_at`. A clock set before 1970 reads as 0:
/// a wrong timestamp is not worth failing a start over.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}
