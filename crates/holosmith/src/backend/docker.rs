//! The Docker backend: the real one, in HS-024.
//!
//! It exists now so the trait has the implementation the product actually ships with, and so
//! `--backend docker` — which is the default (ADR-0001) — answers honestly instead of pretending
//! a session is provisioned. Every operation refuses with [`BackendError::Unsupported`], which
//! the provisioner turns into a `session.status` event and a `status_reason` on the row: an
//! operator who runs this build without `--backend stub` sees `error` with a reason naming the
//! step, not a session that says `starting` forever.
//!
//! Nothing else lives here yet. HS-024 fills `provision` and its four steps, HS-025 the worktree,
//! HS-028 the container, HS-031 the agent; the mount, network and proxy stories add their own.
//! Writing them now would mean guessing at `docker_host`, the projects root and the image tags
//! that those stories own.

use super::{BackendError, SessionBackend, Step, TurnEnd};
use crate::events::EventWriter;
use crate::sessions::Session;

/// The Docker backend. No state yet: `docker_host`, the CLI path and the image tags arrive with
/// HS-024, and an unused field would be dead code until then.
#[derive(Debug, Clone, Copy, Default)]
pub struct DockerBackend;

impl DockerBackend {
    /// Every operation's refusal, in one place so the reason always names the operation.
    fn refuse(operation: &str) -> BackendError {
        BackendError::unsupported("docker", operation)
    }
}

impl SessionBackend for DockerBackend {
    async fn provision(&self, _session: &Session, step: Step) -> Result<(), BackendError> {
        Err(BackendError::unsupported_step(
            step,
            "the docker backend does not provision yet (HS-024)",
        ))
    }

    async fn undo(&self, _session: &Session, step: Step) -> Result<(), BackendError> {
        // Teardown of a step that never ran: there is nothing to remove, and reporting a
        // failure here would turn one refusal into two events.
        let _ = step;
        Ok(())
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

    async fn delete(&self, _session: &Session) -> Result<(), BackendError> {
        Err(Self::refuse("delete"))
    }

    async fn snapshot(&self, _session: &Session) -> Result<(), BackendError> {
        Err(Self::refuse("snapshot"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Status;

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

    #[tokio::test]
    async fn every_operation_refuses_and_says_which_one_it_was() {
        let backend = DockerBackend;
        let session = session();
        // `start_turn` answers how the turn ended; it refuses before it would append anything, so
        // the outcome is discarded and only the refusal is asserted.
        let (_scratch, events) = crate::events::scratch_writer();
        let error = backend
            .provision(&session, Step::Network)
            .await
            .expect_err("the docker backend provisions nothing yet");
        assert_eq!(
            error.to_string(),
            "the network step: the docker backend does not provision yet (HS-024)"
        );
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
            ("delete", backend.delete(&session).await),
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
        // The provisioner undoes what completed; with the docker backend nothing completes, so
        // this path is only reached by a caller that undoes a step anyway.
        DockerBackend
            .undo(&session(), Step::Worktree)
            .await
            .expect("undoing nothing succeeds");
    }
}
