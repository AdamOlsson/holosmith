//! The machine's two proxies, started by one recipe (spec item 2 and item 10 step 4).
//!
//! Two callers have to end with one `hs_egress` and one `hs_key` running on the machine's `egress` network:
//! `serve`'s boot, and `server deploy`'s step 4. ADR-0056 records why the recipe lives here rather than in either
//! of them — the two entry points must start **the same two containers the same way**, and a deployed machine's
//! proxies drifting from a hand-run `serve`'s is the kind of difference nobody would think to compare. Every piece
//! of the recipe is load-bearing and each has its own doc comment below:
//!
//! - **`rm -f` first.** `--restart unless-stopped` means a stopped proxy keeps its name, and a name in use is a
//!   start that fails for a reason about Docker rather than about this machine.
//! - **A bound on every daemon call.** These run on the startup path, where a `docker run` can have to pull an
//!   image first, and a wedged daemon must cost the bound rather than the caller's whole boot ([`START_TIMEOUT`]).
//! - **The `is_file` checks.** `-v` of a path the daemon cannot see creates an empty root-owned *directory*, so a
//!   key file that is not a file is a start that cannot work — said in the words of what is actually
//!   wrong rather than about the container it turned into (ADR-0035's measurement).
//! - **The image is checked first.** Both containers run `holosmith-proxy:<version>`, built by `server deploy`'s
//!   step 3 from the Dockerfile embedded in this binary, and a machine that has never deployed has no
//!   such image: `docker run` would then try to *pull* a name that is not in any registry, which fails with a
//!   sentence about a registry rather than about the step that has not run (ADR-0057).
//! - **The key file is prepared before the directory holding it is mounted**
//!   ([`crate::key::Store::ensure_file`]): a mount source that is not there yet becomes a directory, which the
//!   proxy would then read as TOML.
//!
//! **The two callers disagree about what a failure means, which is why this answers a sentence instead of
//! logging one.** `serve` warns and carries on — a machine without a proxy still serves its control UI, and taking
//! that screen down over a container is the one thing an operator needs not to happen — while `deploy` fails its
//! step, because a deploy reporting `satisfied` over a proxy that never started is exactly the lie its
//! read-then-apply order exists to prevent. One sentence, two policies, and the policy belongs to the caller
//! (ADR-0028's one-spelling rule, applied to a container instead of a check).
//!
//! **A [`Containers`] seam rather than a `Docker`.** Four of the five things a start does are daemon questions, and
//! a test drives the real `docker run` argument vector through a fake that records it — the
//! [`crate::doctor::Machine`] pattern, for the same reason: this crate denies `unsafe` and so cannot set `PATH` or
//! point the CLI at a fake socket, which leaves handing the daemon in as the only way to test a start.

use std::time::Duration;

use crate::config::Config;
use crate::container::{self, EgressProxy, KeyProxy};
use crate::docker::Docker;
use crate::key;

/// How long one daemon call may take before it is "did not start" — the proxy start's bound.
///
/// Read from [`container::PROXY_START_TIMEOUT`] rather than given a second value: it is generous because a first
/// run on a slow machine unpacks an image, and a `serve` boot and a deploy step must give a proxy the same room.
pub const START_TIMEOUT: Duration = container::PROXY_START_TIMEOUT;

/// The two proxies' container names for this run, in the order [`running`] answers: `[egress, key]`.
///
/// A function of the config rather than a constant, because a smoke run's two proxies are its own
/// (`smoke_hs_egress`, `smoke_hs_key`): an unprefixed start would replace the machine's live key proxy — whose mount
/// is *its* key store — with one holding an empty store, and drop the egress proxy's per-session network joins. A
/// caller that names one at a time (`doctor`'s proxy findings, a teardown) composes it the same way from its own
/// config, so no reader of a name spells the prefix itself.
pub fn names(config: &Config) -> [String; 2] {
    [
        container::proxy_name(&config.container_prefix),
        key::key_proxy_name(&config.container_prefix),
    ]
}

/// What a successful start did, for the caller's own log line.
///
/// It is the two facts a start knows and a caller would otherwise re-derive from the config: which container is
/// now running, and how big the egress allow-list it was given is. `serve` logs both at `debug` and says nothing
/// else, because the boot's `info` lines are the eight the startup banner owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    /// The container that is now running, by the name a later `docker inspect` asks for ([`names`]).
    pub container: String,
    /// How many `[egress] allow` domains the container was given.
    ///
    /// The egress proxy's size, which is the one number worth logging about a start; `0` for the key proxy, which
    /// has no allow-list and whose interesting paths are already in [`crate::container::KeyProxy`].
    pub allow: usize,
}

/// The seam: what a caller needs from the daemon to start or read the machine's proxies.
///
/// [`Docker`] is the production implementation, and it is the only one — a fake exists for the tests that must see
/// the argument vector without a daemon (see the module docs).
pub trait Containers {
    /// Whether a container by this name is running, which is the whole of a proxy's state.
    fn running(&self, name: &str) -> bool;

    /// Whether an image is on this machine, which is what a proxy container has to run.
    ///
    /// Asked before either start because the tag is this binary's own (`holosmith-proxy:<version>`) and exists
    /// only once `server deploy`'s step 3 has built it — the alternative is `docker run` attempting a pull of a
    /// repository nobody publishes.
    fn image_exists(&self, image: &str) -> bool;

    /// The names on networks matching `filter`, under `bound` — the question step 4's read asks about `egress`.
    ///
    /// A daemon that cannot answer is an `Err` carrying its sentence: on this path that is a failure, because an
    /// unreadable network is not the same fact as a missing one.
    fn network_names(&self, filter: &str, bound: Duration) -> Result<Vec<String>, String>;

    /// Create the machine's `egress` network if it is not there, which is what step 4 does before it starts the
    /// containers that attach to it.
    fn ensure_network(&self, name: &str) -> Result<(), String>;

    /// Replace a container: force-remove one by this name, then run `args` detached under `bound`.
    ///
    /// One method rather than two, because the order is the contract and both proxies' doc comments explain it:
    /// `--restart unless-stopped` keeps a stopped container's name, so a start without the removal first fails
    /// with Docker's "name already in use" rather than with anything about this machine.
    fn replace(&self, name: &str, args: &[String], bound: Duration) -> Result<(), String>;
}

impl Containers for Docker {
    fn running(&self, name: &str) -> bool {
        // Explicit paths, not `self.running(…)`: an inherent method and a trait method with one name and
        // signature resolve to the trait inside its own impl, which would be infinite recursion.
        Docker::running(self, name)
    }

    fn image_exists(&self, image: &str) -> bool {
        Docker::image_exists(self, image)
    }

    fn network_names(&self, filter: &str, bound: Duration) -> Result<Vec<String>, String> {
        Docker::network_names(self, filter, bound).map_err(|error| error.to_string())
    }

    fn ensure_network(&self, name: &str) -> Result<(), String> {
        // The machine's network is not internal: it is the one place the proxies may reach out from, and
        // `internal` is only ever a session network's flag.
        Docker::ensure_network(self, name, false).map_err(|error| error.to_string())
    }

    fn replace(&self, name: &str, args: &[String], bound: Duration) -> Result<(), String> {
        Docker::remove_container_bounded(self, name, bound);
        match Docker::run_detached(self, args, bound) {
            Ok(Some(_)) => Ok(()),
            // The bound expiring is "the start did not finish", which is not a verdict about the machine but is
            // not a start either: both callers treat it as one failure with one sentence (ADR-0044's rule, one
            // container over).
            Ok(None) => Err(format!(
                "{name} did not start within {}s and was abandoned: a session that needs it will find nothing at \
                 its proxy address",
                bound.as_secs()
            )),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Start the machine's egress proxy container, replacing one an earlier run left behind.
///
/// The only route out of a session's container (spec item 2): one container per machine, attached to the `egress`
/// network and, per session, to that session's own network. It is **replaced on every start** because the
/// allow-list is an argument rather than a mount, so a changed `[egress] allow` takes effect without an operator
/// restarting anything by hand (ADR-0037).
pub fn start_egress(config: &Config, containers: &dyn Containers) -> Result<Started, String> {
    let proxy = EgressProxy::of(config);
    image_there(
        containers,
        &proxy.image,
        "egress proxy",
        "a session's container has no route out without it",
    )?;
    let allow = proxy.allow.len();
    containers.replace(&proxy.name, &proxy.run_args(), START_TIMEOUT)?;
    Ok(Started {
        container: proxy.name,
        allow,
    })
}

/// Start the machine's key proxy container, replacing one an earlier run left behind.
///
/// The only thing in this build that holds a real provider key: it validates a session's dummy key and swaps in
/// the real one, over TLS, from a directory mounted read-only ([`KeyProxy::mount`]). The key file is created
/// (mode 0600) before the start, because it is a mount source and a mount of a missing path is a directory.
pub fn start_key(config: &Config, containers: &dyn Containers) -> Result<Started, String> {
    let store = key::Store::from_data_root(&config.data_root);
    if let Err(error) = store.ensure_file() {
        return Err(format!(
            "no key proxy was started: the key file {} could not be prepared ({error}), and a mount of a missing \
             source is an empty directory",
            store.path().display()
        ));
    }
    let proxy = KeyProxy::of(config);
    image_there(
        containers,
        &proxy.image,
        "key proxy",
        "no session's dummy key can be swapped for the real one without it",
    )?;
    if !proxy.keys.is_file() {
        return Err(format!(
            "no key proxy was started: the key file {} is not a file, and mounting it would give the proxy a \
             directory to read as TOML",
            proxy.keys.display()
        ));
    }
    // **The file and the directory it lives in must have one owner.** The container runs as the key file's owner
    // (`KeyProxy::run_args`) because `--cap-drop ALL` takes root's `DAC_OVERRIDE`, and it must then traverse the
    // mounted *directory*, which is 0700. One `--user` cannot satisfy both when they disagree, and the failure is
    // a container that cannot stat the file it was mounted, exits 1 and restarts forever — which arrives as a
    // network error two layers away. So this refuses with a sentence naming both, because no flag can be right
    // and the operator is the only one who can choose.
    if let (Some(file), Some(directory)) = (
        std::fs::metadata(&proxy.keys).ok(),
        std::fs::metadata(proxy.mount()).ok(),
    ) {
        use std::os::unix::fs::MetadataExt as _;
        if (file.uid(), file.gid()) != (directory.uid(), directory.gid()) {
            return Err(format!(
                "no key proxy was started: {} belongs to {}:{} and the directory it is mounted through, {}, to \
                 {}:{} — the container runs as the file's owner, so it could not read the directory (mode 0700), \
                 and a container that cannot stat its mount exits and restarts forever (`chown -R` one of them)",
                proxy.keys.display(),
                file.uid(),
                file.gid(),
                proxy.mount().display(),
                directory.uid(),
                directory.gid()
            ));
        }
    }
    containers.replace(&proxy.name, &proxy.run_args(), START_TIMEOUT)?;
    Ok(Started {
        container: proxy.name,
        allow: 0,
    })
}

/// `[egress, key]`, in that order: whether each proxy's container is running.
///
/// The read half of the pair above, and the whole of `server deploy` step 4's state read: a container that is
/// running is what makes a second deploy honestly `satisfied` rather than a re-application. `serve` never asks
/// (its boot always replaces them), which is why the answer is a plain pair of booleans.
///
/// The config is the value the starters are handed, and it is what the two names are composed from ([`names`]).
pub fn running(config: &Config, containers: &dyn Containers) -> [bool; 2] {
    let [egress, key] = names(config);
    [containers.running(&egress), containers.running(&key)]
}

/// The one check both starters make before asking the daemon to run anything: the image is on this machine.
///
/// Both proxies run the same tag, so the sentence is one shape with two subjects — the container that was not
/// started, and what the machine is without it. The fix is named as the command rather than as a story, because
/// an operator reading a `serve` boot's warning has a terminal and not a backlog.
fn image_there(
    containers: &dyn Containers,
    image: &str,
    proxy: &str,
    without: &str,
) -> Result<(), String> {
    if containers.image_exists(image) {
        return Ok(());
    }
    Err(format!(
        "no {proxy} was started: the image {image} was not found on this machine (either it is not there or the \
         daemon did not answer), so the container would have nothing to run — `holosmith server deploy`'s step 3 \
         builds it from the Dockerfile embedded in this binary, and {without}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A daemon that owns one image and records every `docker run` it was asked for.
    ///
    /// The two starters ask [`Containers::image_exists`] before they ask for a container, so this is the whole of
    /// what a start needs to be driven without a daemon: the image's presence, and the argv it would have run.
    struct Fake {
        image: bool,
        ran: RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn with_image(image: bool) -> Self {
            Self {
                image,
                ran: RefCell::new(Vec::new()),
            }
        }
    }

    impl Containers for Fake {
        fn running(&self, _name: &str) -> bool {
            false
        }

        fn image_exists(&self, _image: &str) -> bool {
            self.image
        }

        fn network_names(&self, _filter: &str, _bound: Duration) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn ensure_network(&self, _name: &str) -> Result<(), String> {
            Ok(())
        }

        fn replace(&self, name: &str, args: &[String], _bound: Duration) -> Result<(), String> {
            let mut ran = self.ran.borrow_mut();
            ran.push(vec![name.to_string()]);
            ran.last_mut()
                .expect("just pushed")
                .extend(args.iter().cloned());
            Ok(())
        }
    }

    #[test]
    fn a_start_without_the_image_names_the_image_and_the_step_that_builds_it() {
        // The tag is this binary's own version, so it exists only once a deploy's step 3 has built it: without
        // this check `docker run` would try to *pull* a repository nobody publishes, and the caller would report a
        // registry's sentence rather than the step that has not run (ADR-0057).
        let dir = std::env::temp_dir().join(format!("holosmith-proxies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch data root is creatable");
        let config = Config {
            data_root: dir.clone(),
            ..Config::default()
        };

        let absent = Fake::with_image(false);
        for (what, started) in [
            ("egress proxy", start_egress(&config, &absent)),
            ("key proxy", start_key(&config, &absent)),
        ] {
            let sentence = started.expect_err("there is no image for the container to run");
            for fact in [
                what,
                crate::images::proxy_image().as_str(),
                "holosmith server deploy",
                "was not found on this machine",
            ] {
                assert!(
                    sentence.contains(fact),
                    "the refusal does not name {fact}: {sentence}"
                );
            }
        }
        assert!(
            absent.ran.borrow().is_empty(),
            "a start with no image asked the daemon for a container"
        );

        // With the image, each starter runs exactly one container, from that image: the check is a refusal and not
        // a second recipe.
        let present = Fake::with_image(true);
        assert_eq!(
            start_egress(&config, &present)
                .expect("the image is there")
                .container,
            container::PROXY_NAME
        );
        assert_eq!(
            start_key(&config, &present)
                .expect("the image is there")
                .container,
            key::KEY_PROXY_NAME
        );
        let ran = present.ran.borrow();
        assert_eq!(ran.len(), 2, "a start ran {ran:?}");
        for args in ran.iter() {
            assert!(
                args.iter().any(|a| *a == crate::images::proxy_image()),
                "a container was started from another image: {args:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
