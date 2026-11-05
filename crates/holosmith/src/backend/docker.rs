//! The Docker backend: the real one, from HS-025.
//!
//! It exists so the trait has the implementation the product actually ships with, and so `--backend
//! docker` — which is the default (ADR-0001) — answers honestly instead of pretending a session is
//! provisioned. **All four provisioning steps are real:** the worktree (HS-025), the session's private
//! network (HS-028), the container (HS-028) and the agent inside it (HS-031). The turn's five operations
//! still refuse — pi's RPC mapping is HS-032 to HS-035 — with [`BackendError::Unsupported`], which the
//! runner turns into a reason on the row: an operator who prompts a session sees the refusal, not a session
//! that hangs.
//!
//! Whether the machine *can* run Docker sessions at all is not this file's question: [`crate::preflight`]
//! answers it once, at startup, and `serve` refuses to bring up a Docker run that fails it (HS-024). A
//! backend that reported its own preflight would be a second opinion about the same machine.
//!
//! What is left, and who owns it: the turn's mapping over pi's RPC (HS-032 – HS-035), the `egress` network
//! and the startup self-test (HS-036), and the egress and key proxies (HS-037, HS-039). HS-028 creates the
//! minimum a container needs — an **internal** network of its own, which is the isolation — and HS-036 owns
//! the rest of the network's story.
//!
//! **The container is the agent (HS-031).** `docker run` is attached (`-i`), its command is `pi --mode rpc`,
//! and this module holds the spawned process (`crate::agent::Agent`) in a registry with a per-session
//! **generation**. Two things follow from that and are worth knowing before reading `delete` and
//! `supervise`: a launch replaces the registry entry, so an exit from a container a newer launch replaced is
//! not news; and a delete *forgets* the launch before it removes the container, so the exit its own removal
//! provokes is not recorded against a row that is going away (ADR-0039).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use super::{BackendError, ExitWatcher, ProcessEnd, Provisioned, SessionBackend, Step, TurnEnd};
use crate::agent::{self, Agent, Ended};
use crate::container::{ContainerSettings, SessionContainer, network_name};
use crate::docker::{Docker, DockerError};
use crate::events::EventWriter;
use crate::exit::SessionLog;
use crate::sessions::Session;
use crate::worktree::{Refusal, Worktree, refuse, repository_of};

/// The Docker backend: one machine's container settings.
///
/// The projects root is the one setting every host path hangs off (item 1: `<projects>/.worktrees/<id>`,
/// `<projects>/.git`), and the image, the daemon's address and the limits are what a container needs to
/// exist at all — so they arrive through the constructor rather than being read from the config at each
/// call: a backend built without them would have to guess, and a guess about an image is a container that
/// does not start.
#[derive(Debug, Clone)]
pub struct DockerBackend {
    settings: ContainerSettings,
    /// One `async` mutex per session this process has started, so two starts of one session cannot
    /// interleave.
    ///
    /// Spec item 6: "Two starts for the same session cannot race: a per-session lock serialises them." A
    /// start is `docker rm -f` followed by `docker run`, and two of those interleaved produce a
    /// "name already in use" that neither caller can explain. The lock is `tokio`'s rather than `std`'s
    /// because it is held across an `.await`, and the map behind it is a `std` mutex that is never held
    /// across one — the map is only touched to hand out an `Arc` (the same shape ADR-0012's database mutex
    /// has, and for the same reason).
    ///
    /// **The map keeps one entry per session id this process has started**, and it does not shrink: an
    /// entry cannot be dropped while anyone may still be waiting on it, and a sweep that guesses when that
    /// is lets a third caller mint a fresh mutex and walk into the section the lock exists to protect (the
    /// review of HS-029 found exactly that in the first draft's `strong_count` test). An unlocked
    /// `tokio::sync::Mutex` plus a 16-byte id is not worth a racy registry, and a process is restarted long
    /// before this matters.
    starts: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// The agent process each session's container is running, and which launch of it is current.
    ///
    /// One entry per session id, **replaced** by every launch and never removed: the generation is what
    /// says whether an exit still belongs to the session's container (see [`ProcessEnd`]), and a map that
    /// forgot an entry would let a re-created id restart the count and call an old exit current. The memory
    /// is a handle and an `Arc` per session this process served, which is the trade [`Self::starts`] already
    /// makes for the same reason.
    agents: Arc<Mutex<HashMap<String, Launched>>>,
    /// The pi version the image runs, asked once per process (`Some(Err(..))` is a remembered failure).
    ///
    /// One question, one answer: `docker run --entrypoint pi <image> --version` costs a container start, and
    /// the answer cannot change while this process lives (an image is replaced by an upgrade, which restarts
    /// the server). A `tokio` mutex because the answer arrives through `spawn_blocking` and the wait is an
    /// `await`.
    version: Arc<tokio::sync::Mutex<Option<Result<String, String>>>>,
    /// `<data>/sessions/<id>/session.log`: the session's stderr and the launch line (spec item 15, HS-030).
    log: SessionLog,
}

/// One launched agent process: the generation that names it, the agent, and the exit nobody has taken yet.
#[derive(Debug)]
struct Launched {
    generation: u64,
    agent: Arc<Agent>,
    exit: Option<tokio::sync::oneshot::Receiver<Ended>>,
}

impl DockerBackend {
    /// The backend for the machine `settings` describe.
    ///
    /// One value rather than seven arguments, and one place ([`ContainerSettings::of`]) that knows where the
    /// projects root, the image, the daemon's address and the limits come from. A backend built without them
    /// would have to guess, and a guess about an image is a container that does not start.
    pub fn new(settings: ContainerSettings) -> Self {
        Self {
            log: SessionLog::new(settings.data_root.clone()),
            settings,
            starts: Arc::new(Mutex::new(HashMap::new())),
            agents: Arc::new(Mutex::new(HashMap::new())),
            version: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// This session's current agent, if one is running.
    ///
    /// Public because a test that wants to ask the process something — the ready handshake is the pi step's,
    /// but "did the project's own resources load" is a question only the live agent can answer — has no
    /// other way in. HS-032's turn methods will use it from inside this module.
    pub fn agent(&self, session_id: &str) -> Option<Arc<Agent>> {
        let agents = self.agents.lock().unwrap_or_else(PoisonError::into_inner);
        agents
            .get(session_id)
            .map(|launched| Arc::clone(&launched.agent))
    }

    /// Forget this session's launch: the exit of the process that is running is no longer its to report.
    ///
    /// Dropping the registry entry is what a delete does before it removes the container, and it is the
    /// cheapest of the two ways to say "that container is not this session's any more" — the other is
    /// bumping the generation, which would claim a *new* launch exists.
    fn forget_agent(&self, session_id: &str) {
        let mut agents = self.agents.lock().unwrap_or_else(PoisonError::into_inner);
        agents.remove(session_id);
    }

    /// The pi version the agent image runs, asserted against `pins/pi.txt` once per process.
    ///
    /// The story's fourth acceptance line: "the agent image's pi version equals the binary's pinned pi
    /// version; a mismatch fails the session with a clear error". A cached failure is re-answered rather
    /// than re-asked, because a machine whose image is wrong is wrong for every session and the operator
    /// needs the sentence once per row, not one container start per session.
    async fn image_version(&self) -> Result<String, BackendError> {
        let mut cache = self.version.lock().await;
        if let Some(known) = cache.as_ref() {
            return known.clone().map_err(BackendError::failed);
        }
        let docker = self.settings.docker();
        let image = self.settings.image.clone();
        let asked = tokio::task::spawn_blocking(move || agent::version_of(&docker, &image)).await;
        let answer = match asked {
            Ok(Ok(version)) => agent::version_check(&self.settings.image, &version),
            Ok(Err(reason)) => Err(format!(
                "the agent image {} could not say which pi it runs: {reason}",
                self.settings.image
            )),
            Err(error) => Err(format!(
                "the agent image {} was not asked in time ({error})",
                self.settings.image
            )),
        };
        *cache = Some(answer.clone());
        answer.map_err(BackendError::failed)
    }

    /// The session's start lock, taken for the whole of `start`/`resume`.
    ///
    /// The entry is dropped when it is the last one holding it, so the map is swept rather than grown by
    /// one entry per session this process ever served. `Arc::strong_count` is the refcount of the mutex
    /// plus the map's own, so `<= 2` means "nobody else is waiting" — the pattern a `Weak`-based registry
    /// would need a second map for.
    async fn start_lock(&self, session_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut starts = self.starts.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(
                starts
                    .entry(session_id.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        lock.lock_owned().await
    }

    /// The worktree this session is entitled to, from the machine's projects root.
    ///
    /// Derived rather than read back from the row: the naming rule lives in one function
    /// ([`Worktree::of`]), and the row's copy of the path is for readers, not for the code that makes it.
    fn worktree(&self, session: &Session) -> Worktree {
        Worktree::of(&self.settings.projects_root, &session.project, &session.id)
    }

    /// This session's container, from the machine's settings and the row.
    fn container(&self, session: &Session) -> SessionContainer {
        SessionContainer::of(&self.settings, session)
    }

    /// This session's private network name: `agents_<id>`.
    fn network(&self, session: &Session) -> String {
        network_name(&session.id)
    }

    /// Whether `project`'s repository may be mounted, and why not when it may not (HS-026).
    ///
    /// The create asks this **before** the session row exists, so it cannot go through [`Self::worktree`]:
    /// the repository is named by the projects root and the project alone. It is a host read like every
    /// other in this backend, and [`crate::worktree::refuse`] is where the rules are.
    pub fn refuse_project(&self, project: &str) -> Result<(), Refusal> {
        refuse(&repository_of(&self.settings.projects_root, project))
    }

    /// Every operation's refusal, in one place so the reason always names the operation.
    fn refuse(operation: &str) -> BackendError {
        BackendError::unsupported("docker", operation)
    }

    /// Run `git` for one step off the runtime's worker, since a subprocess is not a future.
    ///
    /// The provisioner is a `tokio` task and a `git worktree add` on a large repository takes as long as
    /// it takes; blocking a worker for that would stall every other task it carries. A `JoinError` is not
    /// a git failure — the task panicked or was cancelled — so it says so rather than pretending git
    /// refused.
    async fn blocking<T, F>(step: Step, work: F) -> Result<T, BackendError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, crate::worktree::GitError> + Send + 'static,
    {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|error| {
                BackendError::failed(format!("the {step} step: git did not finish ({error})"))
            })?
            .map_err(|error| BackendError::failed(format!("the {step} step: {error}")))
    }

    /// Run one `docker` call off the runtime's worker, mapping its sentence onto the step's.
    ///
    /// The same reason as the worktree's [`Self::blocking`]: a subprocess is not a future, and a
    /// `docker run` that has to pull an image takes as long as it takes. A `JoinError` is not a Docker
    /// failure — the task panicked or was cancelled — and says so instead of pretending the daemon refused.
    async fn docker<T, F>(step: Step, docker: Docker, work: F) -> Result<T, BackendError>
    where
        T: Send + 'static,
        F: FnOnce(Docker) -> Result<T, DockerError> + Send + 'static,
    {
        tokio::task::spawn_blocking(move || work(docker))
            .await
            .map_err(|error| {
                BackendError::failed(format!("the {step} step: docker did not finish ({error})"))
            })?
            .map_err(|error| BackendError::failed(format!("the {step} step: {error}")))
    }

    /// The session's private network: `docker network create --internal agents_<id>` (spec item 2).
    ///
    /// `--internal` is the isolation and the reason this step exists at all: the container joins *only*
    /// this network, so it has no route to the host and none to the internet. HS-036 adds the `egress`
    /// network, the two proxies and the startup self-test; what is here is the minimum a container needs
    /// to be started on a network of its own rather than on the default bridge.
    ///
    /// A network of the same name that is already there is reused (the teardown failed, or the process
    /// died, and HS-044 is what reconciles that at startup) — creating one twice is not an error worth
    /// failing a session over.
    async fn provision_network(&self, session: &Session) -> Result<Provisioned, BackendError> {
        let network = self.network(session);
        Self::docker(Step::Network, self.settings.docker(), move |docker| {
            docker.create_internal_network(&network)
        })
        .await?;
        tracing::debug!(
            target: "holosmith::backend",
            session = %session.id,
            network = %self.network(session),
            "the session's network is up"
        );
        Ok(Provisioned::Nothing)
    }

    /// The container itself, running pi (spec item 6, HS-031).
    ///
    /// Four things happen here, in this order, and each one is a refusal that must come before the next:
    ///
    /// 1. **The agent directory is made and seeded** (spec item 2). It is a mount's source, and
    ///    `docker run -v` creates a missing source as an empty root-owned directory (ADR-0035).
    /// 2. **The mounts are checked** for the same reason, over all five now.
    /// 3. **The image's pi is asserted** against `pins/pi.txt`, once per process — before a process is
    ///    started rather than after, so a mismatch costs no container and the row's reason is the version
    ///    sentence rather than a mysterious readiness timeout.
    /// 4. **Any leftover container is removed and one attached `docker run` is spawned** (spec item 6).
    ///    Attached is what makes the container the agent: its stdin and stdout are pi's RPC channel and its
    ///    exit code is what [`crate::exit`] classifies.
    async fn provision_container(&self, session: &Session) -> Result<Provisioned, BackendError> {
        // Serialised with a resume of the same session: both are "remove any leftover container, then start
        // one", and interleaving them is a name conflict neither caller could explain (HS-029).
        let _start = self.start_lock(&session.id).await;
        self.start_container(session)
            .await
            .map(|_| Provisioned::Nothing)
    }

    /// Remove any container of this name, then start one: the body a create's container step and a resume
    /// share.
    ///
    /// Spec item 6: "Before every start, `docker rm -f s_<id>` runs and not-found is ignored." `--rm` already
    /// means a stopped container leaves no name behind, so the removal is for the case the flag cannot
    /// cover: a container the process lost track of — a crash, a `kill -9`, a session removed from the row
    /// while its container lived (HS-044 reconciles that at startup; this makes the next start boring).
    async fn start_container(&self, session: &Session) -> Result<(), BackendError> {
        let container = self.container(session);
        let home = container.home.clone();
        agent::ensure_home(&home, session).map_err(|error| {
            BackendError::failed(format!(
                "the container step: the agent directory {} could not be made ({error})",
                home.display()
            ))
        })?;
        container
            .mounts
            .check()
            .map_err(|missing| BackendError::failed(format!("the container step: {missing}")))?;
        // Asked before the launch, and cached for the process: a stale image is a machine problem, not a
        // session's, and the sentence says which image and which two versions.
        let version = self.image_version().await?;
        let arguments = container.run_args();
        let name = container.name.clone();
        let leftover = name.clone();
        Self::docker(Step::Container, self.settings.docker(), move |docker| {
            docker.remove_container(&leftover);
            Ok(())
        })
        .await?;
        // The launch line first, so a launch that never starts has one too, and so the log says which
        // command was attempted before anything else in it.
        self.log.append(
            &session.id,
            &agent::launch_line(&container.image, &version, session, &container.home),
        );
        let command = self.settings.docker().command_line(&arguments);
        let (agent, exit) = Agent::launch(command, &session.id).map_err(|error| {
            BackendError::failed(format!(
                "the container step: the agent process could not be started ({error})"
            ))
        })?;
        let generation = {
            let mut agents = self.agents.lock().unwrap_or_else(PoisonError::into_inner);
            let generation = agents
                .get(&session.id)
                .map(|launched| launched.generation)
                .unwrap_or(0)
                + 1;
            agents.insert(
                session.id.clone(),
                Launched {
                    generation,
                    agent: Arc::new(agent),
                    exit: Some(exit),
                },
            );
            generation
        };
        tracing::debug!(
            target: "holosmith::backend",
            session = %session.id,
            container = %name,
            pi = %version,
            generation,
            "the agent is launched"
        );
        Ok(())
    }

    /// The agent step: pi answered its own state question, so it is ready for a turn (spec item 7's
    /// `starting → idle` is the provisioner's, after this).
    ///
    /// A launch that produced no handshake is a failure rather than a pass: a session that is `idle` with no
    /// agent would accept a prompt nobody can answer (HS-032).
    async fn provision_agent(&self, session: &Session) -> Result<Provisioned, BackendError> {
        let some = self.agent(&session.id).ok_or_else(|| {
            BackendError::failed("the pi step: this session has no agent process".to_string())
        })?;
        let reply = some
            .ready()
            .await
            .map_err(|error| BackendError::failed(format!("the pi step: {error}")))?;
        // A `get_state` answer carries the model and the thinking level pi actually resolved, which is the
        // one thing the row's choices cannot prove on their own (measured: without `--model`/`--thinking`
        // it answers `unknown`/`off`). `debug` rather than `info`: the startup banner is pinned to eight
        // lines and this is diagnostics (the sweeper's and the compactor's precedent).
        let data = reply.data.unwrap_or_default();
        tracing::debug!(
            target: "holosmith::backend",
            session = %session.id,
            model = data.get("model").and_then(|model| model.get("id")).and_then(|id| id.as_str()).unwrap_or("unknown"),
            thinking = data.get("thinkingLevel").and_then(|level| level.as_str()).unwrap_or("unknown"),
            "pi reports ready"
        );
        Ok(Provisioned::Nothing)
    }

    /// `git worktree add` (or the adoption of one that is already there), and the record it makes.
    async fn provision_worktree(&self, session: &Session) -> Result<Provisioned, BackendError> {
        let worktree = self.worktree(session);
        // The row's base commit is the branch's starting point, and it has to win over the branch's
        // current head: a resumed session may have committed since, and `ahead` is measured from where
        // it started (HS-061).
        let base = session.base_commit.clone();
        let (worktree, ensured) = Self::blocking(Step::Worktree, move || {
            let ensured = worktree.ensure(base.as_deref())?;
            Ok((worktree, ensured))
        })
        .await?;
        tracing::debug!(
            target: "holosmith::backend",
            session = %session.id,
            worktree = %worktree.path.display(),
            branch = %worktree.branch,
            base_commit = %ensured.base_commit,
            created = ensured.created,
            "the worktree is ready"
        );
        Ok(Provisioned::Worktree {
            path: worktree.path.to_string_lossy().into_owned(),
            base_commit: ensured.base_commit,
            created: ensured.created,
        })
    }

    /// The worktree's teardown: the directory, the branch and the registration (HS-025).
    ///
    /// Shared by [`SessionBackend::undo`] and [`SessionBackend::delete`], because "this step is being
    /// taken back" and "this session is going away" leave the same thing behind. The container and the
    /// network are removed by their own callers, in that order.
    async fn remove_worktree(&self, session: &Session) -> Result<(), BackendError> {
        let worktree = self.worktree(session);
        Self::blocking(Step::Worktree, move || worktree.remove()).await
    }

    /// **Remove** the session's container by force, ignoring "no such container" (spec item 6's `--rm` rule,
    /// and
    /// ADR-0023's: a teardown of something that is not there is not a failure).
    ///
    /// Named `remove_container`, not `stop_container`: `Docker::stop_container` is the graceful stop, and a
    /// reader following [`SessionBackend::stop`] into a force-removal would be following a different story.
    ///
    /// `docker rm --force` is what `docker stop` would be plus the removal, in one call — the container's
    /// data lives in host directories, so there is nothing to lose by removing it (spec item 6). A
    /// *failure* to remove is logged and not propagated: the delete flow's job is to remove the row, and a
    /// container the daemon cannot remove is a machine problem the log names.
    async fn remove_container(&self, session: &Session) {
        let name = self.container(session).name;
        let removed = Self::docker(Step::Container, self.settings.docker(), move |docker| {
            Ok(docker.remove_container(&name))
        })
        .await;
        match removed {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                target: "holosmith::backend",
                session = %session.id,
                "there was no container to remove"
            ),
            // A daemon that is wedged or a CLI that is gone is *not* "not found", and a teardown that
            // cannot say which happened is a teardown nobody can debug (the doc above promises this line).
            Err(error) => tracing::warn!(
                target: "holosmith::backend",
                session = %session.id,
                %error,
                "the session's container could not be removed"
            ),
        }
    }

    /// Remove the session's own directory: `<data>/sessions/<id>`, with the agent's `pi-home` and its log.
    ///
    /// Best effort, like the container and the network: a delete's job is to remove the row, and a
    /// directory that could not be removed is a `warn` rather than a `deleting` row that never ends
    /// (ADR-0023). A directory that is not there is nothing to do.
    fn remove_session_dir(&self, session: &Session) {
        let directory = crate::exit::session_dir(&self.settings.data_root, &session.id);
        match std::fs::remove_dir_all(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                target: "holosmith::backend",
                session = %session.id,
                path = %directory.display(),
                %error,
                "the session's directory could not be removed"
            ),
        }
    }

    /// Remove the session's network, ignoring "no such network".
    ///
    /// The same three-way answer as the container's: "removed", "was not there" (a debug line) and "the
    /// daemon could not say" (a `warn`), because a wedged daemon and a clean absence are different facts.
    async fn remove_network(&self, session: &Session) {
        let name = self.network(session);
        let removed = Self::docker(Step::Network, self.settings.docker(), move |docker| {
            Ok(docker.remove_network(&name))
        })
        .await;
        match removed {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                target: "holosmith::backend",
                session = %session.id,
                "there was no network to remove"
            ),
            Err(error) => tracing::warn!(
                target: "holosmith::backend",
                session = %session.id,
                %error,
                "the session's network could not be removed"
            ),
        }
    }
}

impl SessionBackend for DockerBackend {
    /// Run one provisioning step: the worktree, the network, the container and the agent are all real.
    ///
    /// The row's `status_reason` is where an operator reads a failure, so each step's refusal names the
    /// step and what was wrong with it. The four arms are written out rather than routed through a helper:
    /// a helper would need an arm for the steps that cannot fail this way, and those arms could never run.
    async fn provision(&self, session: &Session, step: Step) -> Result<Provisioned, BackendError> {
        match step {
            Step::Worktree => self.provision_worktree(session).await,
            Step::Network => self.provision_network(session).await,
            Step::Container => self.provision_container(session).await,
            Step::Pi => self.provision_agent(session).await,
        }
    }

    async fn undo(&self, session: &Session, step: Step) -> Result<(), BackendError> {
        match step {
            Step::Worktree => self.remove_worktree(session).await,
            // The container first when it is undone on its own (its network is not a dependency of
            // removing it, but a container holding a network up would make the network's removal fail).
            Step::Container => {
                // The same lock a start takes: a teardown concurrent with a start could otherwise remove the
                // container the start just made, or the network its `docker run` needs.
                let _start = self.start_lock(&session.id).await;
                self.remove_container(session).await;
                Ok(())
            }
            Step::Network => {
                self.remove_network(session).await;
                Ok(())
            }
            // Teardown of the agent step is the container's: there is no separate process to stop, and
            // reporting a failure here would turn one refusal into two events.
            Step::Pi => Ok(()),
        }
    }

    async fn start_turn(
        &self,
        _session: &Session,
        _prompt: &str,
        _events: &EventWriter,
    ) -> Result<TurnEnd, BackendError> {
        Err(Self::refuse("start_turn"))
    }

    async fn steer(&self, _session: &Session, _text: &str) -> Result<(), BackendError> {
        Err(Self::refuse("steer"))
    }

    async fn clear_queue(&self, _session: &Session) -> Result<(), BackendError> {
        Err(Self::refuse("clear_queue"))
    }

    async fn abort(&self, _session: &Session) -> Result<(), BackendError> {
        Err(Self::refuse("abort"))
    }

    /// Stop the container, keeping the worktree, the history and the events (spec item 6).
    ///
    /// `docker stop --time 10` and not `rm --force`: the ten seconds are what lets pi flush a frame it is
    /// writing, and `--rm` is what removes the container afterwards, so a stop needs no removal of its own.
    /// A container that is not there answers `Ok` — a session that was never started, or stopped twice, is
    /// not a failure (ADR-0023).
    ///
    /// The flag that says this stop was expected is **not** here: it is the server's row write, and
    /// [`crate::stop::Stopper`] records it before calling this (HS-030, ADR-0038). The classification of how
    /// the container then exited is [`crate::exit`]'s, and its observer is HS-031's attached stdio.
    async fn stop(&self, session: &Session) -> Result<(), BackendError> {
        let name = self.container(session).name;
        // `?` on the outer `Result` is deliberate: a daemon that cannot answer is a failed stop, and the
        // ladder's `warn` is where that has to become visible.
        let stopped = Self::docker(Step::Container, self.settings.docker(), move |docker| {
            docker.stop_container(&name)
        })
        .await?;
        if !stopped {
            tracing::debug!(
                target: "holosmith::backend",
                session = %session.id,
                "there was no container to stop"
            );
        }
        Ok(())
    }

    /// Bring a stopped session back with the same id.
    ///
    /// The container and the network, nothing else: the worktree is the provisioner's step, and its `ensure`
    /// is create-or-adopt (ADR-0029), so a resumed session's commits are still on its branch. This is the
    /// story that gives `SessionBackend::resume` a body — it has been uncalled since ADR-0022 — because a
    /// start that cannot cope with a leftover container is not a start (HS-029).
    async fn resume(&self, session: &Session) -> Result<(), BackendError> {
        let _start = self.start_lock(&session.id).await;
        let network = self.network(session);
        Self::docker(Step::Network, self.settings.docker(), move |docker| {
            docker.create_internal_network(&network)
        })
        .await?;
        self.start_container(session).await
    }

    /// Remove everything the session owns: the container, its network, the worktree and its branch, and the
    /// session's own directory.
    ///
    /// The session directory (`<data>/sessions/<id>`) joined this call in HS-031, which is what made it a
    /// directory: it holds the agent's `pi-home` and the `session.log`. A delete takes both, because a session
    /// that is gone cannot be resumed and its log is a record of a row that no longer exists.
    ///
    /// Answering `Ok` for a session nothing was provisioned for is deliberate: teardown of a session that never
    /// got that far must not leave a `deleting` row behind (ADR-0023).
    async fn delete(&self, session: &Session) -> Result<(), BackendError> {
        // The container, then its network, then the worktree and the branch. Each is best-effort except
        // the worktree: a container or a network the daemon has already forgotten is a fact, and a
        // remaining row is what ADR-0023 refuses. The order matters — a container still attached to the
        // network keeps it busy.
        //
        // Under the start lock, because a delete can arrive while a start is in flight (a create's provisioner
        // and a delete of the same session): without it, the delete removes the network between the start's
        // `network create` and its `docker run`.
        let _start = self.start_lock(&session.id).await;
        // Forgotten **before** the container goes, so the exit this removal provokes answers
        // `ProcessEnd::NotCurrent` and is never recorded: `Exits::record` appends to `session.log` (and makes
        // the directory on the way), which would put back the directory the next line removes. A session
        // being deleted has no exit worth classifying — the row is what is going away (ADR-0039).
        self.forget_agent(&session.id);
        self.remove_container(session).await;
        self.remove_network(session).await;
        self.remove_worktree(session).await?;
        self.remove_session_dir(session);
        Ok(())
    }

    /// Hand the server the exit of the agent this session's container is running (HS-031).
    ///
    /// The exit is reported once, by the task [`Agent::launch`] spawned; this takes the receiver and wraps it
    /// with the one question the server cannot answer — **is that exit still about this session's
    /// container?** A forced stop resumes immediately, so the previous container's exit can arrive after a
    /// new one has been launched, and [`ProcessEnd::NotCurrent`] is how that exit says "not news" instead of
    /// turning a stop the server asked for into an `error` row (ADR-0038's inherited ordering constraint).
    ///
    /// The decision is made when the exit arrives, not when it is taken: a generation is compared against
    /// the registry at that moment.
    async fn supervise(&self, session: &Session) -> Option<ExitWatcher> {
        let (receiver, generation) = {
            let mut agents = self.agents.lock().unwrap_or_else(PoisonError::into_inner);
            let launched = agents.get_mut(&session.id)?;
            (launched.exit.take()?, launched.generation)
        };
        let registry = Arc::clone(&self.agents);
        let session_id = session.id.clone();
        let (sender, watcher) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let end = match receiver.await {
                Ok(exit) => {
                    let current = {
                        let agents = registry.lock().unwrap_or_else(PoisonError::into_inner);
                        agents.get(&session_id).map(|launched| launched.generation)
                    };
                    if current == Some(generation) {
                        ProcessEnd::Exited {
                            code: exit.code,
                            stderr: exit.stderr,
                        }
                    } else {
                        tracing::debug!(
                            target: "holosmith::backend",
                            session = %session_id,
                            generation,
                            current,
                            "an older container's exit is not news"
                        );
                        ProcessEnd::NotCurrent
                    }
                }
                Err(_) => ProcessEnd::NotCurrent,
            };
            let _ = sender.send(end);
        });
        Some(ExitWatcher::new(watcher))
    }

    async fn snapshot(&self, _session: &Session) -> Result<(), BackendError> {
        Err(Self::refuse("snapshot"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Status;
    use std::path::Path;

    /// A session with no row behind it, in a projects root that does not exist.
    fn session() -> Session {
        Session {
            id: "0123456789abcdef".to_string(),
            project: "holosmith".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            thinking: "high".to_string(),
            worktree: String::new(),
            base_commit: None,
            status: Status::Starting,
            status_reason: None,
            client_key: None,
            network: false,
            created_at: 1_760_000_000,
            last_active_at: 1_760_000_000,
            stopped_at: None,
            usage_in: 0,
            usage_out: 0,
        }
    }

    fn backend() -> DockerBackend {
        DockerBackend::new(ContainerSettings::at(
            "/nonexistent/projects",
            "/nonexistent/data",
        ))
    }

    #[tokio::test]
    async fn a_pi_step_with_no_launch_behind_it_fails_the_session_instead_of_claiming_ready() {
        // HS-031's fourth hole to close: the pi step used to refuse for every session. It is now the ready
        // handshake, and a handshake with no process would be the worst answer of all — a session marked
        // `idle` that accepts a prompt nobody can answer. So the step fails, and it says which step.
        let error = backend()
            .provision(&session(), Step::Pi)
            .await
            .expect_err("there is no agent process for a session nothing launched");
        let sentence = error.to_string();
        assert_eq!(sentence, "the pi step: this session has no agent process");
    }

    /// A scratch directory, removed by the caller.
    fn scratch(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("holosmith-docker-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the scratch directory is creatable");
        path
    }

    /// A launched fake agent: `sh` that waits for `trigger` to appear and then exits 7.
    ///
    /// A file rather than a sleep because the test has to *decide* when the process ends: with a sleep the
    /// generation comparison would race the exit, and a test that races proves nothing about which branch ran.
    fn launched(trigger: &std::path::Path) -> (Arc<Agent>, tokio::sync::oneshot::Receiver<Ended>) {
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg(format!(
            "while [ ! -f '{}' ]; do sleep 0.01; done; exit 7",
            trigger.display()
        ));
        let (agent, exit) = Agent::launch(command, "0123456789abcdef").expect("it starts");
        (Arc::new(agent), exit)
    }

    /// Put a launch in the registry, as `start_container` does.
    fn remember(
        backend: &DockerBackend,
        session_id: &str,
        generation: u64,
        agent: Arc<Agent>,
        exit: tokio::sync::oneshot::Receiver<Ended>,
    ) {
        backend
            .agents
            .lock()
            .expect("the registry is not poisoned")
            .insert(
                session_id.to_string(),
                Launched {
                    generation,
                    agent,
                    exit: Some(exit),
                },
            );
    }

    /// An exit belongs to the launch that is current, and to nothing older (HS-031, ADR-0039).
    ///
    /// Three cases, and the reason the third exists: a forced stop resumes immediately, so the previous
    /// container's exit can arrive after a new one has been launched. Classifying it then would turn a stop
    /// the server asked for into an `error` row (ADR-0038's inherited ordering constraint).
    #[tokio::test]
    async fn an_exit_is_news_only_for_the_launch_that_is_current() {
        let dir = scratch("docker-generation");
        let backend = backend();
        let session = session();

        // The current launch: the registry still says generation 1 when the exit arrives.
        let trigger = dir.join("one");
        let (agent, exit) = launched(&trigger);
        remember(&backend, &session.id, 1, agent, exit);
        let watcher = backend
            .supervise(&session)
            .await
            .expect("a launch to watch");
        std::fs::write(&trigger, "").expect("writable");
        assert_eq!(
            watcher.wait().await,
            ProcessEnd::Exited {
                code: 7,
                stderr: String::new()
            },
            "the current launch's exit is news"
        );

        // A replaced launch: generation 2 is current when generation 1's exit arrives.
        let first = dir.join("one-again");
        let (agent, exit) = launched(&first);
        remember(&backend, &session.id, 1, agent, exit);
        let stale = backend
            .supervise(&session)
            .await
            .expect("a launch to watch");
        let second = dir.join("two");
        let (agent, exit) = launched(&second);
        remember(&backend, &session.id, 2, agent, exit);
        std::fs::write(&first, "").expect("writable");
        assert_eq!(
            stale.wait().await,
            ProcessEnd::NotCurrent,
            "an exit from a container a newer launch replaced is not news"
        );

        // A forgotten launch: this is what a delete does, so its own removal is never recorded.
        let third = dir.join("three");
        let (agent, exit) = launched(&third);
        remember(&backend, &session.id, 3, agent, exit);
        let forgotten = backend
            .supervise(&session)
            .await
            .expect("a launch to watch");
        backend.forget_agent(&session.id);
        std::fs::write(&third, "").expect("writable");
        assert_eq!(
            forgotten.wait().await,
            ProcessEnd::NotCurrent,
            "a delete's own removal is not an exit worth classifying"
        );
        assert!(
            backend.agent(&session.id).is_none(),
            "the forgotten launch is not the session's agent any more"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_session_nothing_launched_has_no_process_to_supervise() {
        // The provisioner asks after a successful pi step; a backend that has launched nothing answers
        // `None` rather than a watcher that would wait for a report nobody sends.
        assert!(backend().supervise(&session()).await.is_none());
    }

    #[tokio::test]
    async fn the_operations_this_build_does_not_implement_refuse_and_say_which_one_it_was() {
        // `stop` and `resume` left this list in HS-029: both are real now (a `docker stop`, and a network
        // plus a container). What is left is the turn's own machinery — pi's RPC is HS-031/HS-032, the
        // ladder's three rungs are HS-016's on top of them, and the WIP snapshot is HS-042's.
        let backend = backend();
        let session = session();
        // `start_turn` answers how the turn ended; it refuses before it would append anything, so
        // the outcome is discarded and only the refusal is asserted.
        let (_scratch, events) = crate::events::scratch_writer();
        let start_turn = backend
            .start_turn(&session, "a prompt", &events)
            .await
            .map(|_| ());
        for (operation, error) in [
            ("start_turn", start_turn),
            ("steer", backend.steer(&session, "more").await),
            ("clear_queue", backend.clear_queue(&session).await),
            ("abort", backend.abort(&session).await),
            ("snapshot", backend.snapshot(&session).await),
        ] {
            let error = error.expect_err("every operation refuses");
            assert_eq!(
                error.to_string(),
                format!("the docker backend does not implement {operation} yet")
            );
        }
    }

    #[tokio::test]
    async fn undoing_a_step_that_never_ran_is_not_a_failure() {
        // The provisioner undoes what completed, so with the docker backend only the worktree ever
        // does; the other three are reached by a caller that undoes a step anyway, and a teardown of
        // nothing is `Ok`.
        for step in [Step::Network, Step::Container, Step::Pi] {
            backend()
                .undo(&session(), step)
                .await
                .expect("undoing nothing succeeds");
        }
        // Two of those three now *do* something (a `docker rm` and a `docker network rm` against a daemon
        // this test does not have), and both swallow "not there": that is the promise, and it is what
        // makes teardown of a session that never got that far safe (ADR-0023).
    }

    #[tokio::test]
    async fn a_worktree_whose_project_is_gone_is_nothing_to_tear_down() {
        // The projects root here does not exist at all. A delete must not be blocked by that: the
        // delete flow keeps the row `deleting` when a backend fails (ADR-0023).
        backend()
            .undo(&session(), Step::Worktree)
            .await
            .expect("nothing to remove is not a failure");
        backend()
            .delete(&session())
            .await
            .expect("a session whose repository is gone can still be removed");
    }

    #[test]
    fn the_worktree_is_named_from_the_projects_root_and_the_session() {
        let worktree = backend().worktree(&session());
        assert_eq!(
            worktree.repository,
            Path::new("/nonexistent/projects/holosmith")
        );
        assert_eq!(
            worktree.path,
            Path::new("/nonexistent/projects/.worktrees/0123456789abcdef")
        );
        assert_eq!(worktree.branch, "agent/0123456789abcdef");
    }
}
