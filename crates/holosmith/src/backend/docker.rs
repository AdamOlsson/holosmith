//! The Docker backend: the real one, from HS-025.
//!
//! It exists so the trait has the implementation the product actually ships with, and so `--backend
//! docker` — which is the default (ADR-0001) — answers honestly instead of pretending a session is
//! provisioned. **Three of the four provisioning steps are real:** the worktree (HS-025), the session's
//! private network (HS-028) and the container itself (HS-028). The **pi** step still refuses, and so does
//! every lifecycle operation, with [`BackendError::Unsupported`] or [`BackendError::unsupported_step`],
//! which the provisioner turns into a `session.status` event and a `status_reason` on the row: an operator
//! who runs this build without `--backend stub` sees `error` with a reason naming the agent step, not a
//! session that says `starting` forever.
//!
//! Whether the machine *can* run Docker sessions at all is not this file's question: [`crate::preflight`]
//! answers it once, at startup, and `serve` refuses to bring up a Docker run that fails it (HS-024). A
//! backend that reported its own preflight would be a second opinion about the same machine.
//!
//! What is left, and who owns it: the `egress` network and the startup self-test (HS-036), the egress and
//! key proxies (HS-037, HS-039) and the agent inside the container (HS-031). HS-028 creates the minimum a
//! container needs — an **internal** network of its own, which is the isolation — and HS-036 owns the rest
//! of the network's story. HS-031 replaces the detached launch with pi's RPC over the container's stdio.

use super::{BackendError, Provisioned, SessionBackend, Step, TurnEnd};
use crate::container::{ContainerSettings, SessionContainer, network_name};
use crate::docker::{Docker, DockerError};
use crate::events::EventWriter;
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
}

impl DockerBackend {
    /// The backend for the machine `settings` describe.
    ///
    /// One value rather than seven arguments, and one place ([`ContainerSettings::of`]) that knows where the
    /// projects root, the image, the daemon's address and the limits come from. A backend built without them
    /// would have to guess, and a guess about an image is a container that does not start.
    pub fn new(settings: ContainerSettings) -> Self {
        Self { settings }
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

    /// The container itself (spec item 6).
    ///
    /// The mounts are checked *before* the daemon is asked, because `docker run -v` creates a missing
    /// source as an empty root-owned directory: a broken projects root would otherwise give the agent an
    /// empty `/work` and a `.git` that is not the repository, with no error anywhere (ADR-0035).
    ///
    /// HS-031 replaces this launch with an attached one and pipes pi's RPC through the container's stdio;
    /// until then the container is detached and runs the image's own command, which is what makes
    /// "the flags are right" inspectable before there is an agent to run.
    async fn provision_container(&self, session: &Session) -> Result<Provisioned, BackendError> {
        let container = self.container(session);
        container
            .mounts
            .check()
            .map_err(|missing| BackendError::failed(format!("the container step: {missing}")))?;
        let arguments = container.run_args();
        let name = container.name.clone();
        let id = Self::docker(Step::Container, self.settings.docker(), move |docker| {
            docker.run_container(&arguments)
        })
        .await?;
        tracing::debug!(
            target: "holosmith::backend",
            session = %session.id,
            container = %name,
            container_id = %id,
            "the container is running"
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

    /// Remove the session's container, ignoring "no such container" (spec item 6's `--rm` rule, and
    /// ADR-0023's: a teardown of something that is not there is not a failure).
    ///
    /// `docker rm --force` is what `docker stop` would be plus the removal, in one call — the container's
    /// data lives in host directories, so there is nothing to lose by removing it (spec item 6). A
    /// *failure* to remove is logged and not propagated: the delete flow's job is to remove the row, and a
    /// container the daemon cannot remove is a machine problem the log names.
    async fn stop_container(&self, session: &Session) {
        let name = self.container(session).name;
        let removed = Self::docker(Step::Container, self.settings.docker(), move |docker| {
            Ok(docker.remove_container(&name))
        })
        .await;
        if let Ok(false) = removed {
            tracing::debug!(
                target: "holosmith::backend",
                session = %session.id,
                "there was no container to remove"
            );
        }
    }

    /// Remove the session's network, ignoring "no such network".
    async fn remove_network(&self, session: &Session) {
        let name = self.network(session);
        let _ = Self::docker(Step::Network, self.settings.docker(), move |docker| {
            Ok(docker.remove_network(&name))
        })
        .await;
    }
}

impl SessionBackend for DockerBackend {
    /// Run a step that has no implementation yet: one sentence per step, and the sentence names the story
    /// that fills it.
    ///
    /// The row's `status_reason` is where an operator reads this, so "the pi step: the docker backend does
    /// not start the agent yet (HS-031)" says what is wrong, whose work it is, and that the session is not
    /// broken — it is a build that cannot run it. Written out per arm rather than through a helper: a
    /// helper would need an arm for the steps that *are* filled, and those arms could never run.
    async fn provision(&self, session: &Session, step: Step) -> Result<Provisioned, BackendError> {
        match step {
            Step::Worktree => self.provision_worktree(session).await,
            Step::Network => self.provision_network(session).await,
            Step::Container => self.provision_container(session).await,
            Step::Pi => Err(BackendError::unsupported_step(
                step,
                "the docker backend does not start the agent yet (HS-031)",
            )),
        }
    }

    async fn undo(&self, session: &Session, step: Step) -> Result<(), BackendError> {
        match step {
            Step::Worktree => self.remove_worktree(session).await,
            // The container first when it is undone on its own (its network is not a dependency of
            // removing it, but a container holding a network up would make the network's removal fail).
            Step::Container => {
                self.stop_container(session).await;
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

    async fn stop(&self, _session: &Session) -> Result<(), BackendError> {
        Err(Self::refuse("stop"))
    }

    async fn resume(&self, _session: &Session) -> Result<(), BackendError> {
        Err(Self::refuse("resume"))
    }

    /// Remove everything the session owns: the container, its network, the worktree and its branch.
    ///
    /// The session *directory* (`<data>/sessions/<id>`) joins this call in HS-031, which is what makes one.
    /// Answering `Ok` for a session nothing was provisioned for is deliberate: teardown of a session that
    /// never got that far must not leave a `deleting` row behind (ADR-0023).
    async fn delete(&self, session: &Session) -> Result<(), BackendError> {
        // The container, then its network, then the worktree and the branch. Each is best-effort except
        // the worktree: a container or a network the daemon has already forgotten is a fact, and a
        // remaining row is what ADR-0023 refuses. The order matters — a container still attached to the
        // network keeps it busy.
        self.stop_container(session).await;
        self.remove_network(session).await;
        self.remove_worktree(session).await
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
        DockerBackend::new(ContainerSettings::at("/nonexistent/projects"))
    }

    #[tokio::test]
    async fn the_step_this_build_does_not_provision_refuses_and_names_its_story() {
        for (step, story) in [(Step::Pi, "HS-031")] {
            let error = backend()
                .provision(&session(), step)
                .await
                .expect_err("a step with no implementation refuses");
            let sentence = error.to_string();
            assert!(
                sentence.starts_with(&format!("the {step} step: ")),
                "the sentence does not name the step: {sentence}"
            );
            assert!(
                sentence.contains(story),
                "the sentence does not name the story that fills it: {sentence}"
            );
        }
    }

    #[tokio::test]
    async fn every_lifecycle_operation_refuses_and_says_which_one_it_was() {
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
            ("stop", backend.stop(&session).await),
            ("resume", backend.resume(&session).await),
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
