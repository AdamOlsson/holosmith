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
use crate::docker::{Docker, EGRESS_NETWORK};
use crate::events::{self, StreamRegistry, WriterOptions};
use crate::http::{self, App, AppState, Guards};
use crate::logging;
use crate::preview;
use crate::runtime::{Backend, Runtime};
use crate::serve::{self, ServeOptions};
use crate::version::VersionInfo;

/// What the command line asked for.
#[derive(Debug)]
pub struct ServeArgs {
    pub config: Option<PathBuf>,
    pub overrides: Overrides,
    /// The directory the admin socket (`holosmith drain`) lives in; `None` means the machine's own.
    pub admin_dir: Option<PathBuf>,
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
    //
    // The **mode** is `server deploy`'s: 0755. `create_dir_all` uses the process's umask, which is 0775 on a
    // machine whose umask is 0002, and `doctor`'s `perms` finding compares the tree with the modes the deploy
    // writes (HS-049) — so a root this process made would make the *next* boot's `ExecStartPre` fail a finding
    // nothing changed. Only a directory this call created is reset: an operator's own mode is theirs.
    let created = !loaded.config.data_root.exists();
    if let Err(error) = std::fs::create_dir_all(&loaded.config.data_root) {
        eprintln!(
            "holosmith: serve: cannot create the data root {}: {error}",
            loaded.config.data_root.display()
        );
        return ExitCode::FAILURE;
    }
    if created
        && let Err(error) = std::fs::set_permissions(
            &loaded.config.data_root,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
    {
        eprintln!(
            "holosmith: serve: cannot set the data root {} to 0755: {error}",
            loaded.config.data_root.display()
        );
        return ExitCode::FAILURE;
    }
    // The CLI wins over the file, the same rule `--port` and `--data-dir` follow: the file is the
    // machine's setting and the flag is this run's (ADR-0006, ADR-0015). It is written back into
    // the loaded value as well as used here, because an override ADR-0006 applies is applied to
    // the value every later reader sees — otherwise `config.backend` and `runtime.backend` would
    // disagree for exactly one run.
    if let Some(backend) = args.backend {
        loaded.config.backend = backend;
    }
    let backend = loaded.config.backend;
    // The preflight, once, before anything is opened or bound: a machine that cannot run a Docker
    // session must fail here rather than mid-turn (HS-024, spec item 10 §1), and the sentence it fails
    // with is the fix. Every complaint is reported at once — the rule `config validate` follows, because
    // a machine missing two things should take two edits and one restart. A stub run needs no daemon and
    // no free space beyond its own database, so only a Docker run is checked; `doctor` (HS-045) runs the
    // same `preflight::check` on demand, which is what makes the two print the same words.
    //
    // A `hub` is not checked at all: it runs no sessions, and spec item 10 §6's `hub deploy` runs "steps
    // 1 (without the Docker and git checks), 2 (without the docker group), 5, 6 and 7". A hub without a
    // daemon is the normal hub.
    //
    // Since HS-062 the predicate is `disk::guarded`, and it is written there rather than here because three
    // places ask it: this boot, and the two request paths that start work. A create on a full disk answers
    // 507 with its own sentence, and it must agree with this boot about *whether* the floor applies at all —
    // the story's "`doctor` reports the same measurement, so the two never disagree", one level down.
    //
    // It comes after the data root is created because one check is about that filesystem, and before the
    // database is opened because a machine that cannot run sessions should not migrate one.
    if crate::disk::guarded(backend, loaded.config.role) {
        let mut complaints = crate::preflight::check(&loaded.config);
        if complaints.is_empty() {
            tracing::debug!(
                target: "holosmith::serve",
                docker_host = loaded.config.docker_host.as_deref().unwrap_or(crate::preflight::DEFAULT_DOCKER_HOST),
                min_free_gb = loaded.config.min_free_gb,
                "the docker preflight passed"
            );
            // Two things that need a daemon which answered the preflight, and neither of which a stub run or a
            // hub does at all (HS-036, spec item 2).
            //
            // The **egress network** is the machine's own, not a session's: the proxies attached to it are what
            // a container's only route out runs through, so it is created here, once, rather than by each of
            // them. A failure is one `warn` and the boot continues: a session's own network step already fails
            // with a message about itself, and refusing the boot over it would take the control UI down with it
            // — which is the one thing an operator needs to see what happened.
            let docker = Docker::new(loaded.config.docker_host.clone());
            match docker.ensure_network(EGRESS_NETWORK, false) {
                Ok(()) => tracing::debug!(
                    target: "holosmith::serve",
                    network = EGRESS_NETWORK,
                    "the egress network is there"
                ),
                Err(error) => tracing::warn!(
                    target: "holosmith::serve",
                    network = EGRESS_NETWORK,
                    %error,
                    "the egress network could not be created: a session will have no route out through the proxies, and any \
                     step that needs it will say so"
                ),
            }
            // The **egress proxy** on that network: one container per machine, and the only route out a
            // session's container has (HS-037, spec item 2). It is started here rather than per session because
            // it is the machine's; the network step joins it to a session's own network when that session
            // allowed internet access, which is item 2's "topology, not configuration". A failure is one
            // `warn`: a machine without a proxy still serves sessions, and those sessions reach nothing
            // outside their own network — the off case the spec already supports.
            start_egress_proxy(&loaded.config);
            // The **key proxy** on the same network: the only thing that holds a real provider key, joined to
            // every session network by the network step (HS-039). Like the egress proxy it is the machine's,
            // started once and replaced on every boot, and every failure is a `warn`: a machine without it
            // still serves its control UI, and its sessions fail a model call with a sentence rather than
            // refusing to exist.
            start_key_proxy(&loaded.config);
            // The **self-test**: the isolation is enforced by the kernel, so it is measured rather than
            // asserted. Only a *proven* leak refuses the boot; a probe that could not run is a `warn`, because
            // a machine that cannot verify is not the same fact as a machine that was measured to leak (and a
            // machine whose image is not built yet must still serve) — ADR-0044.
            let outcome = crate::selftest::run(&docker, &loaded.config.agent_image);
            if let Some(complaint) = outcome.complaint() {
                complaints.push(complaint);
            }
            match &outcome {
                crate::selftest::Outcome::Passed => tracing::debug!(
                    target: "holosmith::serve",
                    "the network self-test passed: nothing it probed was reachable from an internal network"
                ),
                crate::selftest::Outcome::Unknown(sentence) => tracing::warn!(
                    target: "holosmith::serve",
                    sentence = %sentence,
                    "the network self-test could not run, so this machine's isolation is unverified"
                ),
                // Already in `complaints`, which prints the sentence and refuses.
                crate::selftest::Outcome::Failed(_) => {}
            }
        }
        if !complaints.is_empty() {
            for complaint in &complaints {
                eprintln!("holosmith: serve: {complaint}");
            }
            return ExitCode::FAILURE;
        }
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
    // `--port 0` asks the kernel for a port, so the origin a browser will send is not knowable
    // until after `bind`; the flag decides, because a port written in the file may deliberately
    // name a proxied address rather than the local one.
    let ephemeral_origin = args.overrides.port == Some(0);
    let runtime = Runtime::started(backend);
    let version = VersionInfo::current();
    let listen = loaded.config.control_listen;
    // Read before the async body takes the rest: the admin directory is the run's (`--admin-dir`) or the
    // machine's, and the socket's path is that one value plus `drain::SOCKET_NAME` (HS-051).
    let admin_dir = args
        .admin_dir
        .unwrap_or_else(|| PathBuf::from(crate::deploy::ADMIN_DIR));

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
        // The **preview listener** (HS-041, spec item 3): the second listener, on its own origin, which serves
        // only `/<host>/<id>/*` and no API. It binds here — before the banner, so the banner can print the
        // address the kernel gave, exactly as the control line does — and a bind that fails is one `warn`
        // rather than a refusal: the control UI is the screen an operator needs when something else on the
        // machine already holds the preview port, and `config` already refuses `preview_listen ==
        // control_listen`, which is the collision that is a mistake rather than an accident (ADR-0051).
        let mut preview_listener = None;
        let preview_line = match TcpListener::bind(loaded.config.preview_listen).await {
            Ok(listener) => match listener.local_addr() {
                Ok(address) => {
                    preview_listener = Some(listener);
                    address.to_string()
                }
                Err(error) => {
                    unavailable(&loaded.config, &error);
                    format!("unavailable ({}: {error})", loaded.config.preview_listen)
                }
            },
            Err(error) => {
                unavailable(&loaded.config, &error);
                format!("unavailable ({}: {error})", loaded.config.preview_listen)
            }
        };
        banner(
            &filter,
            &loaded,
            &runtime,
            &version,
            bound,
            &preview_line,
            &database,
        );
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
            backend: Backends::for_run(
                backend,
                crate::container::ContainerSettings::of(&loaded.config),
            ),
            // One flag per process, shared by every clone of this state: the socket's task sets it and the
            // handlers read it (HS-051). Never persisted — a restart is exactly what it exists to precede.
            drain: crate::drain::Control::new(),
            // Rule 2's "one merge per project at a time": one map per process, shared by every clone, so two
            // requests cannot merge into one project at once (HS-054).
            merges: crate::merge::Merges::new(),
            // HS-062's floor measures the data root on the request path, with the same `df` this boot's
            // preflight used: the machine's own program, resolved through `PATH`, which a test swaps for a shim.
            tools: crate::preflight::Tools::default(),
        };
        // The **startup reconcile** (HS-044, V9): what a `kill -9` left behind, put right **before any
        // policy or request can read a row**. It is awaited rather than spawned, and both reasons are
        // measurements:
        //
        // - **Against a create**: the sweep's row snapshot predates the row, so a pass running beside the
        //   first create takes the worktree that create has just made (this story's demo found it). The
        //   listener has already bound, so a client waits instead of being told the wrong thing.
        // - **Against the two policies below**: the sweeper's first pass is immediate and its candidates are
        //   `idle` rows, so it has to start *after* the crash transition has made every live row `starting`,
        //   or it can stop a row the reconcile is about to bring back (the review of this story found it).
        //
        // Awaiting costs a couple of bounded daemon questions and one `git` per project; the *restarts* are
        // provisioner tasks, so provisioning still happens off the serving path. Its own state is a `debug!`
        // line because the banner is the eight lines `tests/serve_cli.rs` pins byte for byte, and the order
        // inside the pass is the contract: the crash transition, then the names no row owns, then the
        // restarts (see `crate::reconcile`).
        tracing::debug!(
            target: "holosmith::serve",
            backend = backend.as_str(),
            "the startup reconcile is running"
        );
        state.reconciler().run().await;
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
        // Compaction is a policy over rows, like the sweeper, and it is wired the same way: built before the
        // state is handed to the router, and its own state is a `debug` line rather than a banner line, for
        // the reason above. Unlike the sweeper it is never absent — the window is the spec's 24 hours and no
        // key turns it off (ADR-0032) — and its first pass is immediate, so a restart folds a day-old log at
        // once rather than a tick later.
        tracing::debug!(
            target: "holosmith::serve",
            after_h = crate::compactor::SNAPSHOT_AFTER.as_secs() / 3600,
            tick_s = crate::compactor::TICK.as_secs(),
            "the compaction task is running"
        );
        let compactor = state.compactor().spawn();
        // The **per-session disk soft cap** (HS-063, spec item 14) joins the two policies above, wired the same
        // way and for the same reason: built before the state is handed to the router, spawned after the
        // reconcile so no row it reads is half-recovered, and absent (`None`) when `session_disk_soft_gb` is
        // `0`. Its own state is a `debug` line rather than a banner line, because the startup contract is the
        // eight lines `tests/serve_cli.rs` pins byte for byte.
        //
        // Its first pass is **not** immediate, unlike the sweeper's: the reconcile has just moved every live row
        // to `starting` and restarted its provisioning, so a pass taken now would measure nothing — and the
        // pass after a restart is the one that catches a session that was already over its cap. The wait is the
        // watchdog's own (`quota::SETTLE`), bounded there rather than here.
        let quota = match state.quota() {
            Some(quota) => {
                tracing::debug!(
                    target: "holosmith::serve",
                    limit_gb = loaded.config.session_disk_soft_gb,
                    tick_s = crate::quota::TICK.as_secs(),
                    "the per-session disk cap's watchdog is running"
                );
                Some(quota.spawn())
            }
            None => {
                tracing::debug!(
                    target: "holosmith::serve",
                    "session_disk_soft_gb is 0, so no session is stopped for its worktree's size"
                );
                None
            }
        };
        // The **stopped-session expiry** (HS-064, spec item 15) is the fourth policy and is wired like the three
        // above: built before the state is handed to the router, spawned after the reconcile so no row it reads is
        // half-recovered, and reported by a `debug` line rather than a banner line — the startup contract is the
        // eight lines `tests/serve_cli.rs` pins byte for byte.
        //
        // Unlike the three it has no off switch (`stopped_ttl_days` refuses `0`), and unlike the disk cap its
        // first pass is **immediate**: the reconcile moves every *live* row to `starting`, and a `stopped` row is
        // not live, so a pass taken now already sees every candidate. That is what makes a daily policy
        // observable — backdate a row, restart, read the row's disappearance.
        tracing::debug!(
            target: "holosmith::serve",
            ttl_days = loaded.config.stopped_ttl_days,
            tick_s = crate::expiry::TICK.as_secs(),
            "the stopped-session expiry is running"
        );
        let expiry = state.expiry().spawn();
        // The **Web Push notification task** (HS-066, spec item 11) is the fifth policy and is wired like the four
        // above: built before the state is handed to the router, spawned after the reconcile, and reported by a
        // `debug` line rather than a banner line — the startup contract is the eight lines `tests/serve_cli.rs` pins
        // byte for byte.
        //
        // It is **always present**: item 11 names no config key that could turn it off, so there is no `None` arm to
        // write. The threshold it prints is the spec's twenty seconds and it is a constant, printed rather than
        // copied — an operator reading this line learns the rule the task applies.
        let notification = state.pusher();
        tracing::debug!(
            target: "holosmith::serve",
            long_turn_s = notification.threshold().as_secs(),
            "the Web Push notification task is running"
        );
        let pusher = notification.spawn();
        // The **admin socket** (HS-051, spec item 10 step 3): the one way to drain this machine, and the one thing
        // in the build that is deliberately *not* an HTTP route — the API table says "not reachable over HTTP",
        // and a second listener outside `App` is what that sentence means. It is bound here, beside the two
        // policies and after the reconcile, so a drain that arrives during boot sees the rows the pass left
        // rather than a half-reconciled machine. A bind that fails is one `warn` and not a refusal, for the
        // preview listener's reason: the control UI is what an operator needs when something else on the machine
        // is wrong. The path is the run's `--admin-dir`, or the machine's `/run/holosmith` (step 2's directory).
        let drain_socket = match crate::drain::listen(&admin_dir) {
            Ok(listener) => Some(crate::drain::spawn(listener, state.drainer())),
            // Every reason is one `warn`, because every one of them means this run has no admin socket; the
            // sentence is what tells them apart (a directory this user cannot write, a socket a live server
            // already owns, a regular file at the path). `serve` keeps serving either way, for the preview
            // listener's reason: the control UI is what an operator needs when something else is wrong.
            Err(sentence) => {
                tracing::warn!(
                    target: "holosmith::serve",
                    admin_dir = %admin_dir.display(),
                    sentence = %sentence,
                    "this run has no admin socket: `holosmith drain` will not find it, and nothing else is affected"
                );
                None
            }
        };
        // The preview listener's own context: the origin an upgrade must come from, the rows, and the one
        // question it cannot answer itself — where a session's container is on its own network (HS-041).
        let preview_context = preview::Context::of(
            &loaded.config,
            Arc::clone(&state.database),
            Arc::new(preview::Containers {
                docker: Docker::new(loaded.config.docker_host.clone()),
                port: loaded.config.preview_upstream_port,
            }),
        );
        // **One signal handler, two listeners.** `serve::run` and `preview::serve` both take a shutdown future
        // and both get theirs from this watch, so the process still has exactly one `StopSignal` and the two
        // drains start together. The task that forwards it is what lets the control listener stay armed on the
        // signal itself, which is the shape `tests/serve_cli.rs` pins.
        let (stopped, stop_watch) = tokio::sync::watch::channel(false);
        let signal = stopped.clone();
        let watcher = tokio::spawn(async move {
            stop.recv().await;
            let _ = signal.send(true);
        });
        let mut preview_stop = stop_watch.clone();
        let preview_task = preview_listener.map(|listener| {
            tokio::spawn(preview::serve(
                listener,
                Arc::new(preview_context),
                async move {
                    let _ = preview_stop.changed().await;
                },
            ))
        });
        let mut control_stop = stop_watch.clone();
        let app = App::new(http::router(state), guards);
        let served = serve::run(listener, app, ServeOptions::default(), async move {
            let _ = control_stop.changed().await;
        })
        .await;
        // A listener that ended on its own — a bind that never answered, a control listener that failed — still
        // stops the other: nothing a session start left behind may outlive the process that serves it.
        let _ = stopped.send(true);
        watcher.abort();
        if let Some(task) = preview_task
            && let Err(error) = task.await
        {
            tracing::warn!(
                target: "holosmith::serve",
                %error,
                "the preview listener stopped unexpectedly"
            );
        }
        // The sweeper is a policy over rows, not part of serving: when the listener's drain is over,
        // the process has no more questions to ask, so its next tick must not hold anything up. The
        // runtime drops the task anyway; aborting says so, and makes the handle's purpose its comment.
        if let Some(sweeper) = sweeper {
            sweeper.abort();
        }
        // Same argument, one step further: compaction writes, so a pass that started must not be allowed to
        // leave a half-written transaction behind. `abort` drops the task at its next await point, and the
        // transaction it was in rolls back on drop — which is also why the fold itself holds no await.
        compactor.abort();
        // And the disk cap's watchdog, for the sweeper's reason: it is a policy over rows rather than part of
        // serving, so its next tick must not hold the process open after the drain is done.
        if let Some(quota) = quota {
            quota.abort();
        }
        // And the stopped-session expiry, for the sweeper's reason and with one of its own: a pass that started
        // must not be allowed to remove a row after the answer promised it was gone — `abort` drops the task at
        // its next await point, and the transition it already wrote stays either way (the delete flow's own
        // task, spawned by it, is a separate one and runs to its end).
        expiry.abort();
        // And the notification task, for the sweeper's reason: it is a reader of the log rather than part of serving,
        // so its next frame must not hold the process open after the drain is done. Nothing it sends can be taken
        // back — a message already POSTed is a browser's notification — which is why aborting is the honest end: the
        // task stops asking, and the messages handed to a push service were never this process's to recall.
        pusher.abort();
        // And the admin socket (HS-051): a drain that started is finished or abandoned with the process, so its
        // accept loop must not hold the runtime open a moment longer than the listeners do. The socket *file* is
        // not removed here — a `serve` that is killed leaves one behind on purpose, and the next boot probes it
        // and unlinks it if it is dead (`drain::listen`), which is the only rule that also covers a `kill -9`.
        if let Some(socket) = drain_socket {
            socket.abort();
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

/// The one `warn` a preview listener that could not bind prints: what it means for this run.
fn unavailable(config: &Config, error: &std::io::Error) {
    tracing::warn!(
        target: "holosmith::serve",
        listen = %config.preview_listen,
        %error,
        "the preview listener could not be bound: this run serves its sessions but no previews, and the control \
         UI is unaffected"
    );
}

/// Start the machine's egress proxy container, replacing one an earlier run left behind (HS-037).
///
/// Called once per Docker boot, after the `egress` network exists, from [`run`]'s startup path — so it is
/// synchronous, and every failure is a `warn` rather than a refusal (see the call site for why). The recipe
/// itself is [`crate::proxies::start_egress`], shared with `server deploy`'s step 4 (ADR-0056), which is why
/// what is left here is the policy that is the boot's own: a `debug` line when it worked and one `warn` naming
/// what this run loses when it did not.
fn start_egress_proxy(config: &Config) {
    let docker = Docker::new(config.docker_host.clone());
    match crate::proxies::start_egress(config, &docker) {
        Ok(started) => tracing::debug!(
            target: "holosmith::serve",
            container = started.container,
            allow = started.allow,
            "the egress proxy is running"
        ),
        Err(sentence) => tracing::warn!(
            target: "holosmith::serve",
            %sentence,
            "the egress proxy is not running: a session that allows internet access will find nothing at its \
             proxy address"
        ),
    }
}

/// Start the machine's key proxy container, replacing one an earlier run left behind (HS-039).
///
/// [`crate::proxies::start_key`] is the recipe, and it is the same one `server deploy`'s step 4 runs (ADR-0056).
/// The policy here is the boot's: a proxy that did not start costs its own `warn` and this run still serves its
/// control UI, and its sessions fail a model call with a sentence rather than refusing to exist.
fn start_key_proxy(config: &Config) {
    let docker = Docker::new(config.docker_host.clone());
    match crate::proxies::start_key(config, &docker) {
        Ok(started) => tracing::debug!(
            target: "holosmith::serve",
            container = started.container,
            "the key proxy is running"
        ),
        Err(sentence) => tracing::warn!(
            target: "holosmith::serve",
            %sentence,
            "the key proxy is not running: a session's model calls will find nothing at its proxy address"
        ),
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
    preview: &str,
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
    // HS-041 binds it, so this line reports the address the kernel gave (a `127.0.0.1:0` preview is how a
    // test asks for one) or, in as many words, that this run has no preview listener and why.
    eprintln!("preview_listen {preview}");
    eprintln!(
        "database {} schema {}",
        database.path().display(),
        database.schema_version()
    );
}
