//! The Docker backend: the real one, from HS-025.
//!
//! It exists so the trait has the implementation the product actually ships with, and so `--backend
//! docker` — which is the default (ADR-0001) — answers honestly instead of pretending a session is
//! provisioned. **The worktree step is real from HS-025 on**; every other operation still refuses with
//! [`BackendError::Unsupported`] or [`BackendError::unsupported_step`], which the provisioner turns into
//! a `session.status` event and a `status_reason` on the row: an operator who runs this build without
//! `--backend stub` sees `error` with a reason naming the step, not a session that says `starting`
//! forever.
//!
//! Whether the machine *can* run Docker sessions at all is not this file's question: [`crate::preflight`]
//! answers it once, at startup, and `serve` refuses to bring up a Docker run that fails it (HS-024). A
//! backend that reported its own preflight would be a second opinion about the same machine.
//!
//! What is left, and who owns it: the session's network and its proxies (HS-027), the container (HS-028)
//! and the agent inside it (HS-031). The mount, network and proxy stories add their own work to the same
//! three steps. Nothing in this file talks to the daemon yet — the worktree is a host program.

use std::path::PathBuf;

use super::{BackendError, Provisioned, SessionBackend, Step, TurnEnd};
use crate::events::EventWriter;
use crate::sessions::Session;
use crate::worktree::{Refusal, Worktree, refuse, repository_of};

/// The Docker backend: the machine's projects root, and nothing else.
///
/// The projects root is the one setting every host path hangs off (item 1: `<projects>/.worktrees/<id>`,
/// `<projects>/<project>/.git`), so it arrives through the constructor rather than being read from the
/// config at each call: a backend built without a path would have to guess one. `docker_host` stays the
/// preflight's and HS-028's; nothing here reads it.
#[derive(Debug, Clone)]
pub struct DockerBackend {
    projects_root: PathBuf,
}

impl DockerBackend {
    /// The backend for a machine whose projects root is `projects_root`.
    pub fn new(projects_root: impl Into<PathBuf>) -> Self {
        Self {
            projects_root: projects_root.into(),
        }
    }

    /// The worktree this session is entitled to, from the machine's projects root.
    ///
    /// Derived rather than read back from the row: the naming rule lives in one function
    /// ([`Worktree::of`]), and the row's copy of the path is for readers, not for the code that makes it.
    fn worktree(&self, session: &Session) -> Worktree {
        Worktree::of(&self.projects_root, &session.project, &session.id)
    }

    /// Whether `project`'s repository may be mounted, and why not when it may not (HS-026).
    ///
    /// The create asks this **before** the session row exists, so it cannot go through [`Self::worktree`]:
    /// the repository is named by the projects root and the project alone. It is a host read like every
    /// other in this backend, and [`crate::worktree::refuse`] is where the rules are.
    pub fn refuse_project(&self, project: &str) -> Result<(), Refusal> {
        refuse(&repository_of(&self.projects_root, project))
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
    /// network join this call when HS-028 has made them.
    async fn remove_worktree(&self, session: &Session) -> Result<(), BackendError> {
        let worktree = self.worktree(session);
        Self::blocking(Step::Worktree, move || worktree.remove()).await
    }
}

impl SessionBackend for DockerBackend {
    /// Run a step that has no implementation yet: one sentence per step, and the sentence names the story
    /// that fills it.
    ///
    /// The row's `status_reason` is where an operator reads this, so "the container step: the docker
    /// backend does not provision the container yet (HS-028)" says what is wrong, whose work it is, and
    /// that the session is not broken — it is a build that cannot run it. Written out per arm rather than
    /// through a helper: a helper would need an arm for the worktree step, and that arm could never run.
    async fn provision(&self, session: &Session, step: Step) -> Result<Provisioned, BackendError> {
        match step {
            Step::Worktree => self.provision_worktree(session).await,
            Step::Network => Err(BackendError::unsupported_step(
                step,
                "the docker backend does not provision the network yet (HS-027)",
            )),
            Step::Container => Err(BackendError::unsupported_step(
                step,
                "the docker backend does not provision the container yet (HS-028)",
            )),
            Step::Pi => Err(BackendError::unsupported_step(
                step,
                "the docker backend does not start the agent yet (HS-031)",
            )),
        }
    }

    async fn undo(&self, session: &Session, step: Step) -> Result<(), BackendError> {
        match step {
            Step::Worktree => self.remove_worktree(session).await,
            // Teardown of a step that never ran: there is nothing to remove, and reporting a failure
            // here would turn one refusal into two events.
            Step::Network | Step::Container | Step::Pi => Ok(()),
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

    /// Remove everything the session owns: the worktree and its branch, from HS-025.
    ///
    /// The container, the session directory and the network are HS-028/HS-031/HS-027's additions to this
    /// call. Answering `Ok` for a session nothing was provisioned for is deliberate: teardown of a
    /// session that never got that far must not leave a `deleting` row behind (ADR-0023).
    async fn delete(&self, session: &Session) -> Result<(), BackendError> {
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
        DockerBackend::new("/nonexistent/projects")
    }

    #[tokio::test]
    async fn the_steps_this_build_does_not_provision_refuse_and_name_their_story() {
        for (step, story) in [
            (Step::Network, "HS-027"),
            (Step::Container, "HS-028"),
            (Step::Pi, "HS-031"),
        ] {
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
