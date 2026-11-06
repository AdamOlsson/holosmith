//! The backend conformance suite (HS-024, spec item 10 §1): what "the backend works" means.
//!
//! Two backends implement [`SessionBackend`], and every story from HS-025 on adds behaviour to one of them.
//! The suite is where that behaviour is defined once and run against both: a case here is a promise the
//! trait makes, not a story's demo, so a backend that adds a step adds its case here and the other backend
//! is asked the same question.
//!
//! Today it holds three promises: **`undo` and `delete` are infallible for a session nothing was
//! provisioned for** (teardown of work that was never done is not a failure), **a failure names the thing
//! it could not do** (that sentence reaches the row's `status_reason` and the transcript), and **a worktree
//! step that fails names the worktree**. HS-028 (container) and HS-031 (pi) add the cases their behaviour
//! needs; a case that only one backend can satisfy is written with a note saying so, never silently
//! skipped — and so far there is none: the worktree step's *effects* are one backend's, and they are
//! tested in `tests/worktree.rs`, while the promise both backends keep about it is the sentence.
//!
//! The suite is generic over the trait rather than `&dyn SessionBackend`, because `async fn` in a trait is
//! not object-safe (ADR-0015).

use holosmith::backend::{BackendError, DockerBackend, Provisioned, SessionBackend, Step};
use holosmith::container::ContainerSettings;
use holosmith::sessions::{Session, Status};
use holosmith::worktree::Retained;

mod support;
use support::{TempDir, fast_stub};

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

/// The one refusal each backend is asked about, and the word the answer must name.
///
/// The two backends refuse differently — the stub fails only where it is told to, the Docker backend
/// refuses every step it has not implemented — so a case cannot spell the call itself.
struct Failures {
    /// A call the backend answers with an error.
    any: Result<(), BackendError>,
    /// The word that error must contain: the step or operation it could not do.
    named: &'static str,
    /// A call to the worktree step that the backend answers with an error.
    worktree: Result<(), BackendError>,
}

/// The whole suite, run against one backend.
async fn conformance<B: SessionBackend>(backend: &B, label: &str, failures: Failures) {
    // Case 1: teardown is infallible, for every step, twice — the provisioner undoes what completed and
    // the delete flow removes what the session owns, and either can be reached for a step that never ran.
    for step in Step::ALL {
        for _ in 0..2 {
            backend
                .undo(&session(), step)
                .await
                .unwrap_or_else(|error| {
                    panic!("{label}: undo of a {step} step that never ran failed: {error}")
                });
        }
    }
    for _ in 0..2 {
        backend
            .delete(&session(), &Retained::nothing())
            .await
            .unwrap_or_else(|error| {
                panic!("{label}: deleting a session that owns nothing failed: {error}")
            });
    }
    // Case 2: a failure names the step or operation it could not do, because the sentence is what the row
    // and the transcript show the operator.
    let error = match failures.any {
        Ok(_) => panic!("{label}: the backend was expected to refuse, and answered Ok"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains(failures.named),
        "{label}: the refusal does not name {:?}: {error}",
        failures.named
    );
    // Case 3: a worktree step that fails says so in its own sentence. Both backends can answer this — the
    // stub by being told to fail there, the Docker backend by having no repository — and the sentence is
    // what the row keeps.
    let error = match failures.worktree {
        Ok(_) => panic!("{label}: the worktree step was expected to refuse, and answered Ok"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("worktree"),
        "{label}: a worktree failure does not name the worktree: {error}"
    );
}

#[tokio::test]
async fn the_stub_backend_passes_the_conformance_suite() {
    // Two stubs, because a stub is told to fail at *one* step: one answers case 2's refusal and the other
    // case 3's. Everything else about them is the same backend.
    let session = session();
    let container = fast_stub().failing_at(Step::Container);
    let worktree = fast_stub().failing_at(Step::Worktree);
    let failures = Failures {
        any: container
            .provision(&session, Step::Container)
            .await
            .map(|_| ()),
        named: "container",
        worktree: worktree
            .provision(&session, Step::Worktree)
            .await
            .map(|_| ()),
    };
    conformance(&container, "stub", failures).await;
}

#[tokio::test]
async fn the_stub_backend_records_no_worktree() {
    // The stub provisions nothing the row names, not even for the worktree step: its job is the lifecycle
    // and a worktree under it would make every test that provisions need a repository (ADR-0015).
    let stub = fast_stub();
    assert_eq!(
        stub.provision(&session(), Step::Worktree)
            .await
            .expect("the stub provisions every step"),
        Provisioned::Nothing
    );
}

#[tokio::test]
async fn the_docker_backend_passes_the_conformance_suite() {
    // A projects root with no repository in it: the worktree step's own failure is what case 3 reads. The
    // agent step is this build's named refusal — the worktree, network and container steps are real from
    // HS-025/HS-028, and each names what it could not do when it fails (`tests/worktree.rs` and
    // `tests/container.rs` are where their effects are asserted).
    let dir = TempDir::new("contract-docker");
    let docker = DockerBackend::new(ContainerSettings::at(
        dir.file("projects"),
        dir.file("data"),
    ));
    let session = session();
    let failures = Failures {
        any: docker.provision(&session, Step::Pi).await.map(|_| ()),
        named: "pi",
        worktree: docker.provision(&session, Step::Worktree).await.map(|_| ()),
    };
    conformance(&docker, "docker", failures).await;
}
