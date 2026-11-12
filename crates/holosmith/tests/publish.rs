//! The publish over a real bare repository: the push (spec item 12's publish paragraph), its refusals,
//! and the credential assertion the story's last acceptance bullet asks for.
//!
//! Item 12's publish is a *remote's* fact, so these tests own a **bare repository** as `origin` — `git init
//! --bare <dir>` and `git remote add origin <dir>` — which makes a real push possible with no network, no daemon
//! and no credential. `git rev-parse`/`refs/heads` **in that repository** is then the measurement, and the reader
//! is the test's own git rather than the value the code just answered.
//!
//! Three of the decisions are only provable by measuring the thing that would otherwise happen:
//!
//! - **the hook test** writes a real `pre-push` hook in the project's common hooks directory and asserts its
//!   marker never appears, which is the only way to show that `-c core.hooksPath=/dev/null` is load-bearing
//!   rather than decorative (the merge's `post-merge` test, one command further out);
//! - **the tag test** sets the *repository's* `push.followTags = true` and asserts no `refs/tags/*` reaches the
//!   remote — dropping `--no-follow-tags` pushes the tag (measured), which is ADR-0062's "a repository config
//!   must not change what a request does" applied to somebody else's repository;
//! - **the moved-on test** rewinds the session's branch after a publish and asserts the remote did not move,
//!   which is "never force-pushes" as a fact about a ref and not about a flag list.
//!
//! The credential assertion is deliberately **two halves in one test**: the session's container carries no git
//! credential (no mount, no environment variable, no seeded file), and the push above ran on the host into a
//! directory under this test's own `TempDir` — a path no container's mount vector names. The absence is what the
//! acceptance asks for; the presence is the half that can fail.
//!
//! The wire group at the end boots the real router over the same repository, because a `TempDir` cannot prove
//! what a client sees: the three keys, the destination's name, and every refusal as status **and** code.

use std::path::PathBuf;
use std::time::Duration;

use axum::http::Method;
use holosmith::clock;
use holosmith::config::{Config, Merge, Publish};
use holosmith::container::{ContainerSettings, SessionContainer};
use holosmith::http::{self, AppState};
use holosmith::publish::{PublishError, Pushed, Request, publish};
use holosmith::sessions::NewSession;
use holosmith::worktree::Worktree;
use serde_json::Value;

mod support;
use support::{Served, TempDir, branch_exists, git, head, repository_on, request, state_in};

/// The drain deadline each served test gives `serve`.
const GRACE: Duration = Duration::from_millis(100);

/// `Config::default().control_origin`, which is what the default guards expect.
const ORIGIN: &str = "http://localhost:8080";

/// One session's repository and the bare remote it publishes to.
///
/// Built the way the provisioner builds it — `git worktree add -b agent/<id> <path> <base>` and one
/// `set_worktree` — and then driven directly, which is what lets a test read the **remote's** refs with its own
/// git rather than with anything this build returned.
struct Fixture {
    // Held for the lifetime of the fixture: the database, the repository, the remote and the worktree live
    // under it.
    _dir: TempDir,
    state: AppState,
    projects: PathBuf,
    repo: PathBuf,
    remote: PathBuf,
    worktree: PathBuf,
    id: String,
    base: String,
}

impl Fixture {
    /// A repository on `main` with one commit, a bare remote called `origin`, a session row and its worktree.
    fn new(label: &str) -> Self {
        Self::with(label, &["origin"], &["main"])
    }

    /// The same, with the two config lists the route reads spelled out: `publish.remotes` and `merge.targets`.
    fn with(label: &str, remotes: &[&str], targets: &[&str]) -> Self {
        let dir = TempDir::new(label);
        let projects = dir.file("projects");
        let repo = projects.join("holosmith");
        repository_on(&repo, "main");
        let base = head(&repo);
        // `origin` is a **bare repository under this test's own TempDir**: a push needs no network, no daemon
        // and no credential, and the refs git writes there are the measurement.
        let remote = dir.file("remote.git");
        std::fs::create_dir_all(&remote).expect("the bare remote directory is creatable");
        git(&remote, &["init", "--quiet", "--bare"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("a UTF-8 temp path"),
            ],
        );
        let config = Config {
            data_root: dir.path().to_path_buf(),
            projects_root: projects.clone(),
            publish: Publish {
                remotes: remotes.iter().map(|name| name.to_string()).collect(),
            },
            merge: Merge {
                targets: targets.iter().map(|name| name.to_string()).collect(),
            },
            ..Config::default()
        };
        let state = state_in(&dir, config);
        let id = {
            let database = state.database.lock().expect("the lock is not poisoned");
            database
                .sessions()
                .create(
                    &NewSession {
                        project: "holosmith".to_string(),
                        model: "claude-sonnet-4.5".to_string(),
                        thinking: "high".to_string(),
                        network: false,
                        client_key: Some(format!("publish-{label}")),
                        base_commit: None,
                    },
                    clock::unix_now(),
                )
                .expect("a session is created")
                .id
        };
        let worktree = projects.join(".worktrees").join(&id);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                &format!("agent/{id}"),
                worktree.to_str().expect("a UTF-8 temp path"),
                &base,
            ],
        );
        {
            let database = state.database.lock().expect("the lock is not poisoned");
            database
                .sessions()
                .set_worktree(&id, worktree.to_str().expect("a UTF-8 temp path"), &base)
                .expect("the row records its worktree");
        }
        Self {
            _dir: dir,
            state,
            projects,
            repo,
            remote,
            worktree,
            id,
            base,
        }
    }

    fn branch(&self) -> String {
        format!("agent/{}", self.id)
    }

    fn worktree(&self) -> Worktree {
        Worktree::of(&self.projects, "holosmith", &self.id)
    }

    /// The answer `POST …/publish` would give, with this fixture's two config lists.
    fn run(&self, request: &Request) -> Result<Pushed, PublishError> {
        publish(
            &self.worktree(),
            request,
            &self.state.config.publish.remotes,
            &self.state.config.merge.targets,
        )
    }

    /// The request item 12's sentence describes: our branch, to `remote`, under its own name.
    fn plain(&self, remote: &str) -> Request {
        Request {
            remote: remote.to_string(),
            branch: None,
        }
    }

    /// Write `path` in the worktree and commit it on the session branch.
    fn commit(&self, path: &str, contents: &[u8], message: &str) {
        let full = self.worktree.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("the directory is creatable");
        }
        std::fs::write(&full, contents).expect("the file is written");
        git(&self.worktree, &["add", path]);
        git(&self.worktree, &["commit", "--quiet", "-m", message]);
    }

    /// The commit a ref points at in the **project** repository, read by the test.
    fn rev(&self, reference: &str) -> String {
        git(&self.repo, &["rev-parse", reference])
    }

    /// Every branch ref the **remote** holds, as `refs/heads/…` — the measure of "the push did (not) land".
    fn remote_heads(&self) -> Vec<String> {
        let listing = git(
            &self.remote,
            &["for-each-ref", "--format=%(refname)", "refs/heads"],
        );
        let mut heads: Vec<String> = listing
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        heads.sort();
        heads
    }

    /// The commit the remote's `reference` points at, read in the bare repository.
    fn remote_rev(&self, reference: &str) -> String {
        git(&self.remote, &["rev-parse", reference])
    }

    async fn served(&self) -> Served {
        Served::start(http::router(self.state.clone()), GRACE).await
    }
}

/// `POST /api/sessions/{id}/publish` with a body, the way the app's own client sends it.
async fn post_publish(addr: std::net::SocketAddr, id: &str, body: &str) -> support::Response {
    request(
        addr,
        &format!(
            "POST /api/sessions/{id}/publish HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {ORIGIN}\r\n\
             Content-Type: application/json\r\nX-Holosmith-Client: 1\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        ),
    )
    .await
}

fn envelope(response: &support::Response) -> Value {
    serde_json::from_str(&response.body).expect("the envelope is JSON")
}

// --- the push ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_publish_lands_the_branch_on_a_bare_remote_and_names_the_commit_it_read() {
    // The demo as an assertion: `{remote:"origin", confirm:true}` with no `branch` pushes `agent/<id>` to
    // `agent/<id>`, and the remote's own git says so afterwards. The answer's `commit` is the branch's tip **as
    // the push found it** — read by the test from the project repository, not echoed from the push's words.
    let fixture = Fixture::new("publish-wire");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    assert!(fixture.remote_heads().is_empty(), "the remote starts empty");
    let served = fixture.served().await;
    let response = post_publish(
        served.addr,
        &fixture.id,
        r#"{"remote":"origin","confirm":true}"#,
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    assert_eq!(body["remote"], "origin");
    assert_eq!(body["branch"], fixture.branch().as_str());
    assert_eq!(body["commit"], tip.as_str());
    assert_eq!(body.as_object().expect("an object").len(), 3);
    // and it is `touches_ci`-free: the CI flag is the changes view's and the warning is that story's, so an
    // answer carrying a flag this route never computed would be a claim about a diff it did not read.
    assert!(body.get("touches_ci").is_none(), "{}", response.body);
    // The remote's own git is the measurement: the ref exists, and it is the commit the answer named.
    assert_eq!(
        fixture.remote_heads(),
        vec![format!("refs/heads/{}", fixture.branch())]
    );
    assert_eq!(
        fixture.remote_rev(&format!("refs/heads/{}", fixture.branch())),
        tip
    );
    // ... and the local branch did not move, because a publish pushes and does not land anything.
    assert_eq!(fixture.rev(&fixture.branch()), tip);
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn the_branch_field_names_the_remote_ref_and_creates_no_local_branch() {
    // `branch` is the **destination**: the ref pushed from stays `agent/<id>` (item 12's "the server pushes the
    // session branch"), and the thing `branch` names is what appears in somebody else's repository. A build that
    // read it as "a second local branch" would fail on `git push origin pr/one` with no such local ref.
    let fixture = Fixture::new("publish-destination");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let tip = fixture.rev(&fixture.branch());
    let served = fixture.served().await;
    let response = post_publish(
        served.addr,
        &fixture.id,
        r#"{"remote":"origin","branch":"pr/one","confirm":true}"#,
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).expect("the answer is JSON");
    assert_eq!(body["branch"], "pr/one");
    assert_eq!(body["commit"], tip.as_str());
    assert_eq!(
        fixture.remote_heads(),
        vec!["refs/heads/pr/one".to_string()]
    );
    assert_eq!(fixture.remote_rev("refs/heads/pr/one"), tip);
    // No local `pr/one` was made, and `agent/<id>` is still where it was.
    assert!(
        !branch_exists(&fixture.repo, "refs/heads/pr/one"),
        "the destination is the remote's ref, not a local branch"
    );
    assert_eq!(fixture.rev(&fixture.branch()), tip);
    served.finish().await.expect("the server stops");
}

#[test]
fn a_branch_with_nothing_beyond_its_base_still_publishes() {
    // A session that has committed nothing has a branch at its base commit, and item 12's sentence is about the
    // branch rather than about the work: "nothing to publish" is `ahead`'s question (and the Changes sheet's),
    // not this route's. The measurement is that the remote gains the branch at that commit.
    let fixture = Fixture::new("publish-empty");
    assert_eq!(fixture.rev(&fixture.branch()), fixture.base);
    let pushed = fixture
        .run(&fixture.plain("origin"))
        .expect("the push lands");
    assert_eq!(pushed.commit, fixture.base);
    assert_eq!(
        fixture.remote_rev(&format!("refs/heads/{}", fixture.branch())),
        fixture.base
    );
}

#[test]
fn a_pre_push_hook_does_not_run_on_the_host() {
    // Item 12's "so that your hooks never run agent-written code", one command further out than the merge's
    // `post-merge`: a `pre-push` hook is a program the host would run with the agent's branch in its argv.
    // `-c core.hooksPath=/dev/null` is the whole defence, and dropping it makes this test red (measured).
    let fixture = Fixture::new("publish-hooks");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let hooks = fixture.repo.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).expect("the hooks directory is creatable");
    let marker = fixture._dir.file("pre-push-ran");
    let hook = hooks.join("pre-push");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .expect("the hook is writable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("the hook is executable");
    }
    let pushed = fixture
        .run(&fixture.plain("origin"))
        .expect("the push lands");
    assert!(
        !marker.exists(),
        "the project's pre-push hook ran on the host"
    );
    assert_eq!(
        fixture.remote_rev(&format!("refs/heads/{}", fixture.branch())),
        pushed.commit
    );
}

#[test]
fn a_repository_that_sets_push_follow_tags_pushes_no_tag() {
    // `--no-follow-tags` is not decoration: a repository's `push.followTags = true` makes `git push` carry
    // annotated tags reachable from the pushed ref **in addition** (measured on this machine). A repository
    // config must not decide what this route writes into somebody else's repository — ADR-0062's `diff.external`
    // rule, applied to a push.
    let fixture = Fixture::new("publish-tags");
    fixture.commit("a.txt", b"a\n", "the session's change");
    git(&fixture.repo, &["config", "push.followTags", "true"]);
    git(&fixture.worktree, &["tag", "-a", "-m", "a tag", "v1"]);
    fixture
        .run(&fixture.plain("origin"))
        .expect("the push lands");
    let tags = git(
        &fixture.remote,
        &["for-each-ref", "--format=%(refname)", "refs/tags"],
    );
    assert_eq!(tags, "", "the push carried a tag: {tags}");
    assert!(branch_exists(
        &fixture.remote,
        &format!("refs/heads/{}", fixture.branch())
    ));
}

#[test]
fn a_remote_that_has_moved_on_is_a_refusal_and_the_remote_does_not_move() {
    // "Never force-pushes", as a fact about a ref: the branch is rewound after a publish, so the remote holds a
    // commit the branch no longer has — the exact shape a force-push would overwrite. The answer is the remote's
    // rejection (not a machine failure), and the remote's ref is byte-for-byte where it was.
    let fixture = Fixture::new("publish-moved-on");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let first = fixture.rev(&fixture.branch());
    fixture
        .run(&fixture.plain("origin"))
        .expect("the first push lands");
    let reference = format!("refs/heads/{}", fixture.branch());
    assert_eq!(fixture.remote_rev(&reference), first);
    fixture.commit("b.txt", b"b\n", "a second change");
    let second = fixture.rev(&fixture.branch());
    fixture
        .run(&fixture.plain("origin"))
        .expect("the second push lands");
    assert_eq!(fixture.remote_rev(&reference), second);
    // Rewind: the remote's tip is now a commit this branch does not have.
    git(&fixture.worktree, &["reset", "--hard", &first]);
    assert_eq!(fixture.rev(&fixture.branch()), first);
    let refused = fixture
        .run(&fixture.plain("origin"))
        .expect_err("a non-fast-forward is refused");
    match &refused {
        // The variant is the claim: a rejected push is *the remote's answer*, not a `500` carrying git's prose.
        PublishError::Rejected {
            remote,
            branch,
            reason,
        } => {
            assert_eq!(remote, "origin");
            assert_eq!(branch, &fixture.branch());
            assert!(!reason.is_empty(), "the rejection lost git's reason");
        }
        other => panic!("a non-fast-forward was not a rejection: {other:?}"),
    }
    assert_eq!(
        fixture.remote_rev(&reference),
        second,
        "a refused push moved the remote"
    );
}

// --- the refusals ----------------------------------------------------------------------------------------

#[tokio::test]
async fn a_protected_destination_is_refused_and_nothing_is_pushed() {
    // Item 12's three protected names, on the **destination**. The remote is empty before and after each, which is
    // the half a status-only assertion would miss.
    let fixture = Fixture::new("publish-protected");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let served = fixture.served().await;
    for (label, branch) in [
        ("main", "main"),
        ("master", "master"),
        ("release", "release/1.0"),
    ] {
        let response = post_publish(
            served.addr,
            &fixture.id,
            &format!(r#"{{"remote":"origin","branch":"{branch}","confirm":true}}"#),
        )
        .await;
        assert_eq!(response.status, 422, "{label}: {}", response.body);
        assert_eq!(
            envelope(&response)["code"],
            "protected_branch",
            "{label}: {}",
            response.body
        );
    }
    assert!(
        fixture.remote_heads().is_empty(),
        "a refused publish pushed"
    );
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn the_configured_target_is_refused_with_its_own_code_and_protection_wins_first() {
    // Two facts, two codes: a destination called `trunk` is nobody's protected name and is still this machine's
    // merge target ("push the target from the host"), while `main` on a machine that merges into `main` is the
    // **protected** refusal — the constant is checked first (ADR-0066 D2), so the answer names the rule that is
    // true of every machine.
    let target = Fixture::with("publish-target", &["origin"], &["trunk"]);
    target.commit("a.txt", b"a\n", "the session's change");
    let served = target.served().await;
    let response = post_publish(
        served.addr,
        &target.id,
        r#"{"remote":"origin","branch":"trunk","confirm":true}"#,
    )
    .await;
    assert_eq!(response.status, 422, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "target_not_pushable");
    assert!(target.remote_heads().is_empty());
    served.finish().await.expect("the server stops");

    let both = Fixture::with("publish-target-main", &["origin"], &["main"]);
    both.commit("a.txt", b"a\n", "the session's change");
    let served = both.served().await;
    let response = post_publish(
        served.addr,
        &both.id,
        r#"{"remote":"origin","branch":"main","confirm":true}"#,
    )
    .await;
    assert_eq!(response.status, 422, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "protected_branch");
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn an_empty_publish_remotes_disables_the_route_for_every_id() {
    // Item 12's "empty by default, so the endpoint is disabled", answered as a `403` **before** the session is
    // read: the endpoint is off for every id, so an id the table does not hold must not be what this route says
    // first. A `404` for a path in the table would also be a lie about the table.
    let fixture = Fixture::with("publish-disabled", &[], &["main"]);
    fixture.commit("a.txt", b"a\n", "the session's change");
    let served = fixture.served().await;
    for (label, id) in [
        ("known", fixture.id.as_str()),
        ("unknown", "0123456789abcdef"),
    ] {
        let response = post_publish(served.addr, id, r#"{"remote":"origin","confirm":true}"#).await;
        assert_eq!(response.status, 403, "{label}: {}", response.body);
        assert_eq!(
            envelope(&response)["code"],
            "publish_disabled",
            "{label}: {}",
            response.body
        );
    }
    assert!(fixture.remote_heads().is_empty());
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn an_unlisted_remote_and_one_the_repository_lacks_are_one_code_and_two_sentences() {
    // `bad_remote`'s two facts, and the needles are each half's *own*: the config's list for the first, the
    // repository for the second. A single needle that both sentences contain would pass with either half
    // deleted (S1).
    let unlisted = Fixture::new("publish-unlisted");
    unlisted.commit("a.txt", b"a\n", "the session's change");
    let served = unlisted.served().await;
    let response = post_publish(
        served.addr,
        &unlisted.id,
        r#"{"remote":"upstream","confirm":true}"#,
    )
    .await;
    assert_eq!(response.status, 422, "{}", response.body);
    let body = envelope(&response);
    assert_eq!(body["code"], "bad_remote");
    assert!(
        body["message"]
            .as_str()
            .expect("a sentence")
            .contains("this machine pushes to no other"),
        "{}",
        response.body
    );
    served.finish().await.expect("the server stops");

    // The same code for a name the *config* lists and the **repository** has no remote for, with the sentence
    // that describes that half.
    let configured = Fixture::with("publish-no-remote", &["origin", "upstream"], &["main"]);
    configured.commit("a.txt", b"a\n", "the session's change");
    let served = configured.served().await;
    let response = post_publish(
        served.addr,
        &configured.id,
        r#"{"remote":"upstream","confirm":true}"#,
    )
    .await;
    assert_eq!(response.status, 422, "{}", response.body);
    let body = envelope(&response);
    assert_eq!(body["code"], "bad_remote");
    assert!(
        body["message"]
            .as_str()
            .expect("a sentence")
            .contains("the repository has no such remote"),
        "{}",
        response.body
    );
    assert!(configured.remote_heads().is_empty());
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn confirm_is_required_and_false_pushes_nothing() {
    // Item 12's `confirm:true`: absent (or a non-boolean) is the body's shape, `false` is a request that asked not
    // to push — a refusal rather than a `200` no-op, because the request carried a decision.
    let fixture = Fixture::new("publish-confirm");
    fixture.commit("a.txt", b"a\n", "the session's change");
    let served = fixture.served().await;
    for (label, body, status, code) in [
        ("absent", r#"{"remote":"origin"}"#, 422, "bad_body"),
        (
            "not a bool",
            r#"{"remote":"origin","confirm":"yes"}"#,
            422,
            "bad_body",
        ),
        (
            "false",
            r#"{"remote":"origin","confirm":false}"#,
            422,
            "confirm_required",
        ),
    ] {
        let response = post_publish(served.addr, &fixture.id, body).await;
        assert_eq!(response.status, status, "{label}: {}", response.body);
        assert_eq!(
            envelope(&response)["code"],
            code,
            "{label}: {}",
            response.body
        );
        assert!(
            fixture.remote_heads().is_empty(),
            "{label}: a refused publish pushed"
        );
    }
    served.finish().await.expect("the server stops");
}

#[tokio::test]
async fn the_state_refusals_are_this_route_s_sentences() {
    // All four of the changes view's `no_worktree` facts, at the wire, with **publish**'s words: no recorded
    // worktree, a worktree directory that is gone, the branch gone, and a project repository that is gone. The
    // code is shared and the sentences are not — a publish that said "there is nothing to review" would describe
    // another request.
    let fresh = Fixture::new("publish-no-base");
    let id = {
        let database = fresh
            .state
            .database
            .lock()
            .expect("the lock is not poisoned");
        database
            .sessions()
            .create(
                &NewSession {
                    project: "holosmith".to_string(),
                    model: "claude-sonnet-4.5".to_string(),
                    thinking: "high".to_string(),
                    network: false,
                    client_key: Some("publish-no-base-session".to_string()),
                    base_commit: None,
                },
                clock::unix_now(),
            )
            .expect("a second session is created")
            .id
    };
    let served = fresh.served().await;
    let response = post_publish(served.addr, &id, r#"{"remote":"origin","confirm":true}"#).await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "no_worktree");
    assert!(
        envelope(&response)["message"]
            .as_str()
            .expect("a sentence")
            .contains("no branch to publish"),
        "{}",
        response.body
    );
    // An id the table does not hold is the envelope's `404`, not this route's problem.
    let unknown = post_publish(
        served.addr,
        "0123456789abcdef",
        r#"{"remote":"origin","confirm":true}"#,
    )
    .await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    assert_eq!(envelope(&unknown)["code"], "not_found");
    served.finish().await.expect("the server stops");

    let gone = Fixture::new("publish-worktree-gone");
    gone.commit("a.txt", b"a\n", "the session's change");
    std::fs::remove_dir_all(&gone.worktree).expect("the worktree is removable");
    let served = gone.served().await;
    let response = post_publish(
        served.addr,
        &gone.id,
        r#"{"remote":"origin","confirm":true}"#,
    )
    .await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "no_worktree");
    assert!(
        envelope(&response)["message"]
            .as_str()
            .expect("a sentence")
            .contains("resume the session"),
        "{}",
        response.body
    );
    served.finish().await.expect("the server stops");

    // The fourth fact: the branch itself is gone while the directory and its registration are intact — what a
    // `git update-ref -d` (or a hand-run `branch -D` in another checkout) leaves behind. Without this arm the
    // `NoBranch` sentence is a line nothing measures, which an earlier development note records as the merge's own lesson.
    let gone_branch = Fixture::new("publish-branch-gone");
    gone_branch.commit("a.txt", b"a\n", "the session's change");
    git(
        &gone_branch.repo,
        &[
            "update-ref",
            "-d",
            &format!("refs/heads/{}", gone_branch.branch()),
        ],
    );
    let served = gone_branch.served().await;
    let response = post_publish(
        served.addr,
        &gone_branch.id,
        r#"{"remote":"origin","confirm":true}"#,
    )
    .await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "no_worktree");
    assert!(
        envelope(&response)["message"]
            .as_str()
            .expect("a sentence")
            .contains("the session's branch is gone"),
        "{}",
        response.body
    );
    assert!(gone_branch.remote_heads().is_empty());
    served.finish().await.expect("the server stops");

    let missing = Fixture::new("publish-repository-gone");
    missing.commit("a.txt", b"a\n", "the session's change");
    std::fs::remove_dir_all(&missing.repo).expect("the repository is removable");
    let served = missing.served().await;
    let response = post_publish(
        served.addr,
        &missing.id,
        r#"{"remote":"origin","confirm":true}"#,
    )
    .await;
    assert_eq!(response.status, 409, "{}", response.body);
    assert_eq!(envelope(&response)["code"], "no_worktree");
    assert!(
        envelope(&response)["message"]
            .as_str()
            .expect("a sentence")
            .contains("restore the project's checkout"),
        "{}",
        response.body
    );
    served.finish().await.expect("the server stops");
}

#[test]
fn the_publish_route_is_mutating_and_in_the_manifest() {
    // A `POST`, so the manifest records `Mutating` and every guard applies — the property that keeps a route which
    // writes somebody else's repository behind ADR-0010's three headers. The kind is asked of the app rather than
    // assumed.
    let fixture = Fixture::new("publish-guards");
    let app = http::App::new(
        http::router(fixture.state.clone()),
        http::Guards::from_config(&fixture.state.config),
    );
    let entry = app
        .manifest()
        .iter()
        .find(|entry| entry.path == "/api/sessions/{id}/publish")
        .expect("the route is in the manifest");
    assert_eq!(entry.kind, http::RouteKind::Mutating);
    assert_eq!(entry.method, Method::POST);
}

// --- the credential assertion -----------------------------------------------------------------------------

#[test]
fn a_session_container_carries_no_git_credential() {
    // The story's last acceptance bullet, and the one assertion about something that is *not* there. Three reads,
    // because a credential could reach a container three ways and each has its own shape:
    //
    // 1. the mount vector — five mounts, and none of them a home directory, a `.ssh`, a `.gitconfig`, a
    //    `.git-credentials`, a `.netrc`, the key store or the secrets directory;
    // 2. the environment — no `GIT_*`, no `SSH_*`, no askpass variable;
    // 3. the agent directory the server seeds — exactly one file, `settings.json`, whose keys are the model and
    //    the thinking level.
    //
    // The positive half is the pushes above: they land in a bare repository under a `TempDir`, a path no mount
    // vector here names, which is what makes "the host did it" a measurement rather than a claim.
    let fixture = Fixture::new("publish-credential");
    let session = {
        let database = fixture
            .state
            .database
            .lock()
            .expect("the lock is not poisoned");
        database
            .sessions()
            .get(&fixture.id)
            .expect("the row is readable")
            .expect("the row exists")
    };
    let settings =
        ContainerSettings::at(fixture.projects.clone(), fixture._dir.path().to_path_buf());
    let container = SessionContainer::of(&settings, &session);
    let arguments = container.run_args();

    // 1. The mounts: `-v host:at[:ro]`, five of them (spec item 1's four plus pi's own directory).
    let mounts: Vec<&String> = arguments
        .iter()
        .enumerate()
        .filter(|(index, _)| index > &0 && arguments[index - 1] == "-v")
        .map(|(_, argument)| argument)
        .collect();
    assert_eq!(
        mounts.len(),
        5,
        "the mount vector is not the spec's five: {arguments:?}"
    );
    for mount in &mounts {
        for forbidden in [
            ".ssh",
            ".gitconfig",
            ".git-credentials",
            ".netrc",
            ".config/git",
            "keys.toml",
            "vapid.toml",
            "/secrets",
        ] {
            assert!(
                !mount.contains(forbidden),
                "the container mounts {forbidden}: {mount}"
            );
        }
    }
    // The project's own `.git/config` **is** mounted, read-only: it is the repository's configuration, which is
    // not a credential, and the read-only flag is what keeps the agent from adding one.
    let config_mount = mounts
        .iter()
        .find(|mount| mount.contains(".git/config"))
        .expect("the project's config is one of the mounts");
    assert!(config_mount.ends_with(":ro"), "{config_mount}");

    // 2. The environment: every `--env KEY=VALUE`, and no credential-ish name among them.
    let environment: Vec<&String> = arguments
        .iter()
        .enumerate()
        .filter(|(index, _)| index > &0 && arguments[index - 1] == "--env")
        .map(|(_, argument)| argument)
        .collect();
    assert!(!environment.is_empty(), "the container has no environment");
    for variable in &environment {
        let name = variable.split('=').next().unwrap_or_default();
        for forbidden in ["GIT_", "SSH_", "ASKPASS", "CREDENTIAL"] {
            assert!(
                !name.to_ascii_uppercase().contains(forbidden),
                "the container carries {variable}"
            );
        }
    }

    // 3. The seed: one file, two keys, no secret.
    let home = holosmith::agent::home(fixture._dir.path(), &fixture.id);
    holosmith::agent::ensure_home(&home, &session).expect("the agent directory is seeded");
    let mut seeded: Vec<String> = std::fs::read_dir(&home)
        .expect("the agent directory is readable")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    seeded.sort();
    assert_eq!(
        seeded,
        vec!["settings.json".to_string()],
        "the seed wrote more than pi's settings"
    );
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join("settings.json")).expect("the seed is readable"),
    )
    .expect("the seed is JSON");
    let keys: Vec<&String> = settings.as_object().expect("an object").keys().collect();
    assert_eq!(keys.len(), 2, "the seed gained a field: {settings}");
    for key in keys {
        assert!(
            key == "defaultModel" || key == "defaultThinkingLevel",
            "the seed carries {key}"
        );
    }
}

/// The publish's own `sentence()` is the log's, and the two lists it names are the operator's facts.
#[test]
fn every_refusal_names_the_fact_it_is_about() {
    use holosmith::publish::PROTECTED;

    assert_eq!(PROTECTED, &["main", "master", "release/"]);
    let cases = [
        (
            PublishError::Unlisted {
                remote: "upstream".to_string(),
                remotes: vec!["origin".to_string()],
            },
            "upstream",
        ),
        (
            PublishError::NoRemote {
                remote: "origin".to_string(),
            },
            "origin",
        ),
        (
            PublishError::Protected {
                branch: "main".to_string(),
            },
            "main",
        ),
        (
            PublishError::Target {
                branch: "trunk".to_string(),
                targets: vec!["trunk".to_string()],
            },
            "trunk",
        ),
        (
            PublishError::Rejected {
                remote: "origin".to_string(),
                branch: "agent/x".to_string(),
                reason: "non-fast-forward".to_string(),
            },
            "non-fast-forward",
        ),
    ];
    for (error, needle) in cases {
        let sentence = error.sentence();
        assert!(
            sentence.contains(needle),
            "{sentence} does not name {needle}"
        );
    }
}
