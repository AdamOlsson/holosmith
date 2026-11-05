//! `holosmith proxy egress`: the egress proxy, as a mode of this binary (HS-037, spec item 2).
//!
//! The proxy is not a second program. The spec's "Internal subcommands, used inside containers" lists
//! `holosmith proxy egress`, and one binary means the proxy container runs the same artifact the server does —
//! so there is nothing to build for it, nothing to keep in step, and one `pins/pi.txt`-grade invariant fewer
//! (ADR-0045).
//!
//! The allow-list comes from `--allow`, one domain per flag, and from the config file's `[egress] allow` when
//! the flag is absent. The container the server starts passes the flags, because a container has no config
//! file; a host-side run reads the operator's own list, which is what makes the demo and a `doctor` check
//! (HS-045) one command rather than two spellings of the same policy.
//!
//! Nothing here binds a privileged port by design: the container's listen address is `0.0.0.0:3128` because
//! that is what [`crate::egress::DEFAULT_LISTEN`] says and what `HTTPS_PROXY` names, and a host-side run is
//! told where to listen.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::config::{Config, LoadError};
use crate::egress::{self, Rules};
use crate::logging;

/// What the command line asked for.
#[derive(Debug, Clone)]
pub struct ProxyArgs {
    /// Where to listen; `None` is [`egress::DEFAULT_LISTEN`].
    pub listen: Option<String>,
    /// The allowed domains. Empty means "read `[egress] allow` from the config file".
    pub allow: Vec<String>,
    /// The config file, for the allow-list fallback.
    pub config: Option<PathBuf>,
}

pub fn run(args: ProxyArgs) -> ExitCode {
    logging::init();
    let listen = args
        .listen
        .clone()
        .unwrap_or_else(|| egress::DEFAULT_LISTEN.to_string());
    let address: SocketAddr = match listen.parse() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("holosmith: proxy egress: {listen} is not an address to listen on: {error}");
            return ExitCode::FAILURE;
        }
    };
    let allow = if args.allow.is_empty() {
        match Config::load(args.config.clone()) {
            Ok(loaded) => loaded.config.egress.allow.clone(),
            Err(LoadError::Invalid(problems)) => {
                eprint!("{problems}");
                return ExitCode::FAILURE;
            }
            Err(error) => {
                eprintln!(
                    "holosmith: proxy egress: no --allow was given and the config could not be read: {error}"
                );
                return ExitCode::FAILURE;
            }
        }
    } else {
        args.allow.clone()
    };
    let rules = Arc::new(Rules::new(allow));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("holosmith: proxy egress: cannot start a runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let listener = match TcpListener::bind(address).await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("holosmith: proxy egress: cannot listen on {address}: {error}");
                return ExitCode::FAILURE;
            }
        };
        let bound = listener.local_addr().unwrap_or(address);
        tracing::info!(
            target: "holosmith::egress",
            listen = %bound,
            allow = rules.allow().len(),
            "the egress proxy is listening"
        );
        // The two signals a container runtime stops this process with. A proxy has nothing to drain — a tunnel
        // in flight is the client's, not this process's — so the signal ends the loop and the process.
        let stop = match crate::serve::StopSignal::install() {
            Ok(stop) => stop,
            Err(error) => {
                eprintln!("holosmith: proxy egress: cannot install the signal handler: {error}");
                return ExitCode::FAILURE;
            }
        };
        tokio::select! {
            _ = egress::serve(listener, rules) => {}
            _ = stop.recv() => {
                tracing::info!(
                    target: "holosmith::egress",
                    "the egress proxy was asked to stop"
                );
            }
        }
        ExitCode::SUCCESS
    })
}
