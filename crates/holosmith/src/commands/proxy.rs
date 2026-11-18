//! `holosmith proxy egress|key|mcp`: the machine's three proxies, each as a mode of this binary (spec item 2).
//!
//! A proxy is not a second program. The spec's "Internal subcommands, used inside containers" lists
//! `holosmith proxy egress` and `holosmith proxy key`, and one binary means a proxy container runs the same artifact
//! the server does — so there is nothing to build for it, nothing to keep in step, and one `pins/pi.txt`-grade
//! invariant fewer (ADR-0045). `proxy mcp` is the third and the same argument (ADR-0099).
//!
//! The allow-list comes from `--allow`, one domain per flag, and from the config file's `[egress] allow` when
//! the flag is absent. The container the server starts passes the flags, because a container has no config
//! file; a host-side run reads the operator's own list, which is what makes the demo and a `doctor` check
//!  one command rather than two spellings of the same policy.
//!
//! Nothing here binds a privileged port by design: the container's listen address is `0.0.0.0:3128` because
//! that is what [`crate::egress::DEFAULT_LISTEN`] says and what `HTTPS_PROXY` names, and a host-side run is
//! told where to listen. The two keyed proxies are the same shape over one key store: `0.0.0.0:8080` for the key
//! proxy ([`crate::keyproxy::DEFAULT_LISTEN`]) and `0.0.0.0:8090` for the MCP proxy
//! ([`crate::mcpproxy::DEFAULT_LISTEN`]), with `--keys` naming the store instead of `--allow` naming a list.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::config::{Config, LoadError};
use crate::egress::{self, Rules};
use crate::key::Store;
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

/// What `holosmith proxy key` and `holosmith proxy mcp` take: both key-swapping proxies read one key store and are
/// told where to listen, so the flags have one spelling.
#[derive(Debug, Clone)]
pub struct KeyedProxyArgs {
    pub listen: Option<String>,
    pub keys: Option<PathBuf>,
    pub config: Option<PathBuf>,
}

/// Which key-swapping proxy this run is.
///
/// The two differ in three values and nothing else: the address they listen on by default, the target their lines go
/// to, and the words a refusal carries. The rest — read the key file's path from `--keys` or the config, prepare a
/// store that is not there, bind, and stop on a signal — is one path, because a second copy of it is a second place a
/// proxy could start without the key file it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Key,
    Mcp,
}

impl Which {
    /// The words a refusal carries and a log message names: `proxy key` / `proxy mcp`.
    fn command(self) -> &'static str {
        match self {
            Self::Key => "proxy key",
            Self::Mcp => "proxy mcp",
        }
    }

    fn default_listen(self) -> &'static str {
        match self {
            Self::Key => crate::keyproxy::DEFAULT_LISTEN,
            Self::Mcp => crate::mcpproxy::DEFAULT_LISTEN,
        }
    }

    /// The one `info` line a start prints, once the listener is bound. Two arms rather than a target and a message
    /// because `tracing`'s `target` is a constant: the target is the module an operator greps for, so it cannot be a
    /// value chosen at run time.
    fn log_listening(self, bound: SocketAddr, keys: &std::path::Path) {
        match self {
            Self::Key => tracing::info!(
                target: "holosmith::keyproxy",
                listen = %bound,
                keys = %keys.display(),
                "the key proxy is listening"
            ),
            Self::Mcp => tracing::info!(
                target: "holosmith::mcpproxy",
                listen = %bound,
                keys = %keys.display(),
                "the MCP proxy is listening"
            ),
        }
    }

    /// The one `info` line a stop prints.
    fn log_stopping(self) {
        match self {
            Self::Key => {
                tracing::info!(target: "holosmith::keyproxy", "the key proxy was asked to stop")
            }
            Self::Mcp => {
                tracing::info!(target: "holosmith::mcpproxy", "the MCP proxy was asked to stop")
            }
        }
    }
}

/// `holosmith proxy key`: the key proxy, as a mode of this binary.
pub fn run_key(args: KeyedProxyArgs) -> ExitCode {
    run_keyed(Which::Key, args)
}

/// `holosmith proxy mcp`: the MCP proxy, as a mode of this binary.
pub fn run_mcp(args: KeyedProxyArgs) -> ExitCode {
    run_keyed(Which::Mcp, args)
}

fn run_keyed(which: Which, args: KeyedProxyArgs) -> ExitCode {
    logging::init();
    let command = which.command();
    let listen = args
        .listen
        .clone()
        .unwrap_or_else(|| which.default_listen().to_string());
    let address: SocketAddr = match listen.parse() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("holosmith: {command}: {listen} is not an address to listen on: {error}");
            return ExitCode::FAILURE;
        }
    };
    let path = match args.keys {
        Some(path) => path,
        None => match Config::load(args.config.clone()) {
            Ok(loaded) => loaded.config.secrets_path(),
            Err(LoadError::Invalid(problems)) => {
                eprint!("{problems}");
                return ExitCode::FAILURE;
            }
            Err(error) => {
                eprintln!(
                    "holosmith: {command}: no --keys was given and the config could not be read: {error}"
                );
                return ExitCode::FAILURE;
            }
        },
    };
    let store = std::sync::Arc::new(Store::new(path));
    // A container gets the file as a read-only mount, so its parent is not a directory it may chmod. Only a
    // host-side run (no file yet, or the proxy's own first start) creates and seals it.
    if !store.path().is_file()
        && let Err(error) = store.ensure_file()
    {
        eprintln!("holosmith: {command}: the key store could not be prepared: {error}");
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("holosmith: {command}: cannot start a runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let listener = match TcpListener::bind(address).await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("holosmith: {command}: cannot listen on {address}: {error}");
                return ExitCode::FAILURE;
            }
        };
        let bound = listener.local_addr().unwrap_or(address);
        which.log_listening(bound, store.path());
        let stop = match crate::serve::StopSignal::install() {
            Ok(stop) => stop,
            Err(error) => {
                eprintln!("holosmith: {command}: cannot install the signal handler: {error}");
                return ExitCode::FAILURE;
            }
        };
        match which {
            Which::Key => tokio::select! {
                _ = crate::keyproxy::serve(listener, store) => {}
                _ = stop.recv() => which.log_stopping(),
            },
            Which::Mcp => tokio::select! {
                _ = crate::mcpproxy::serve(listener, store) => {}
                _ = stop.recv() => which.log_stopping(),
            },
        }
        ExitCode::SUCCESS
    })
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
