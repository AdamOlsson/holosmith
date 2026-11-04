//! The backend conformance suite (HS-024, spec item 10 §1): what "the backend works" means.
//!
//! Two backends implement [`SessionBackend`], and every story from HS-025 on adds behaviour to one of them.
//! The suite is where that behaviour is defined once and run against both: a case here is a promise the
//! trait makes, not a story's demo, so a backend that adds a step adds its case here and the other backend
//! is asked the same question.
//!
//! Today it holds the two promises that survive this early a stage — **`undo` is infallible for every
//! step**, because teardown of work that was never done is not a failure, and **a failure names the thing
//! it could not do**, because that sentence reaches the row's `status_reason` and the transcript. HS-025
//! (worktree), HS-028 (container) and HS-031 (pi) add the cases their behaviour needs; a case that only one
//! backend can satisfy today is written with a `Failures` table saying so, never silently skipped.
//!
//! The suite is generic over the trait rather than `&dyn SessionBackend`, because `async fn` in a trait is
//! not object-safe (ADR-0015).

use holosmith::backend::{BackendError, DockerBackend, SessionBackend, Step};
use holosmith::sessions::{Session, Status};

mod support;
use support::fast_stub;

/// A session, spelled once: every case needs one and none of them may create a row.
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

/// The whole suite, run against one backend.
///
/// `refused` is the caller's proof that a failing call *does* fail, and `named` is the word its error must
/// contain: a backend refuses differently (the stub fails only where it is told to, the Docker backend
/// refuses everything), so the case is "the error names what it could not do" and the caller supplies the
/// call that makes one.
async fn conformance<B: SessionBackend>(
    backend: &B,
    label: &str,
    refused: Result<(), BackendError>,
    named: &str,
) {
    // Case 1: `undo` is infallible, for every step, on a session nothing was ever provisioned for.
    for step in Step::ALL {
        backend
            .undo(&session(), step)
            .await
            .unwrap_or_else(|error| {
                panic!("{label}: undo of a {step} step that never ran failed: {error}")
            });
    }
    // Case 2: a failure names the step or operation it could not do, because the sentence is what the row
    // and the transcript show the operator.
    let error = match refused {
        Ok(()) => panic!("{label}: the backend was expected to refuse, and answered Ok"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains(named),
        "{label}: the refusal does not name {named:?}: {error}"
    );
}

#[tokio::test]
async fn the_stub_backend_passes_the_conformance_suite() {
    let session = session();
    let stub = fast_stub().failing_at(Step::Container);
    let refused = stub.provision(&session, Step::Container).await;
    conformance(&stub, "stub", refused, "container").await;
}

#[tokio::test]
async fn the_docker_backend_passes_the_conformance_suite() {
    let session = session();
    let docker = DockerBackend;
    let refused = docker.provision(&session, Step::Network).await;
    conformance(&docker, "docker", refused, "network").await;
}
