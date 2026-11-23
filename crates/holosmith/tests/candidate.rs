//! The candidate as a real repository answers it: `[update] source`'s `master`, its tag and its cleanliness.
//!
//! No daemon, no database and no request: this is git, a temporary checkout and the four answers a promotion's
//! page, request and report all read. The refusals that depend on a real `git` cannot be measured without one,
//! which is why they are here and the pure rules (the sentences, the truncation count, the staleness comparison)
//! are in the module's own tests.

use std::path::Path;

use holosmith::candidate::{self, Candidate, Refusal};
use holosmith::config::Config;

mod support;
use support::{TempDir, git, head, repository_on};

/// A config whose `[update] source` is `source`.
fn config(source: &Path) -> Config {
    let mut config = Config::default();
    config.update.source = Some(source.to_path_buf());
    config
}

#[test]
fn a_clean_master_is_the_candidate_and_describe_names_its_tag() {
    let dir = TempDir::new("candidate-clean");
    repository_on(dir.path(), "master");

    // No tag yet: `describe --always` answers the abbreviated commit, and that is what the release would be named.
    let value = candidate::resolve(&config(dir.path())).expect("a clean master is a candidate");
    assert_eq!(value.source, dir.path());
    assert_eq!(value.commit, head(dir.path()));
    assert_eq!(
        value.tag,
        git(dir.path(), &["describe", "--tags", "--always"])
    );

    // A tag is the name the release directory gets, and it is carried unedited.
    git(dir.path(), &["tag", "v1.2.3"]);
    assert_eq!(
        candidate::resolve(&config(dir.path())).expect("the tag does not make it unpromotable"),
        Candidate {
            source: dir.path().to_path_buf(),
            commit: head(dir.path()),
            tag: "v1.2.3".to_string(),
        }
    );
}

#[test]
fn a_directory_that_is_not_a_repository_is_refused() {
    let dir = TempDir::new("candidate-not-a-repository");
    std::fs::write(dir.file("README.md"), "not a checkout\n").expect("the file is writable");

    let refusal = candidate::resolve(&config(dir.path()))
        .expect_err("a plain directory has no `master` to read");
    match &refusal {
        Refusal::NotARepository { source, .. } => assert_eq!(source, dir.path()),
        other => panic!("a plain directory answered {other:?}"),
    }
    assert!(refusal.to_string().contains("git clone <url>"), "{refusal}");
    assert!(
        refusal.to_string().contains("-src`"),
        "the repair clones beside the path a promotion cannot read: {refusal}"
    );
}

#[test]
fn a_repository_on_main_is_refused_by_its_head() {
    let dir = TempDir::new("candidate-on-main");
    repository_on(dir.path(), "main");

    let refusal = candidate::resolve(&config(dir.path()))
        .expect_err("`HEAD` on `main` is not the candidate's checkout");
    match &refusal {
        Refusal::NotOnMaster { source, head } => {
            assert_eq!(source, dir.path());
            assert_eq!(head.as_deref(), Some("refs/heads/main"));
        }
        other => panic!("a repository on main answered {other:?}"),
    }
    let sentence = refusal.to_string();
    assert!(sentence.contains("refs/heads/main"), "{sentence}");
    assert!(
        sentence.contains("git -C ") && sentence.contains("checkout master"),
        "{sentence}"
    );
}

#[test]
fn a_master_branch_that_is_not_checked_out_is_refused() {
    // Both branches exist and `HEAD` is on `main`: `rev-parse refs/heads/master` answers the right commit, while
    // `status` and `describe` are about `main`, so the commit and the bytes would be two different trees.
    let dir = TempDir::new("candidate-master-not-checked-out");
    repository_on(dir.path(), "main");
    git(dir.path(), &["branch", "master"]);

    let refusal = candidate::resolve(&config(dir.path()))
        .expect_err("`master` existing is not `master` checked out");
    match &refusal {
        Refusal::NotOnMaster { head, .. } => assert_eq!(head.as_deref(), Some("refs/heads/main")),
        other => panic!("a repository on main with a master branch answered {other:?}"),
    }
}

#[test]
fn a_detached_head_is_refused() {
    let dir = TempDir::new("candidate-detached");
    repository_on(dir.path(), "master");
    git(dir.path(), &["checkout", "--quiet", "--detach"]);

    let refusal =
        candidate::resolve(&config(dir.path())).expect_err("a detached `HEAD` is no branch at all");
    match &refusal {
        Refusal::NotOnMaster { source, head } => {
            assert_eq!(source, dir.path());
            assert_eq!(head, &None);
        }
        other => panic!("a detached HEAD answered {other:?}"),
    }
    assert!(
        refusal.to_string().contains("`HEAD` is detached"),
        "{refusal}"
    );
}

#[test]
fn a_master_with_no_commit_has_nothing_to_promote() {
    // `HEAD` is on `refs/heads/master` and the branch has no commit, which is the one state left for the branch
    // that has nothing behind it: every other one is refused by the checkout question.
    let dir = TempDir::new("candidate-no-commit");
    git(dir.path(), &["init", "--quiet", "-b", "master"]);

    let refusal =
        candidate::resolve(&config(dir.path())).expect_err("an unborn `master` has no commit");
    match &refusal {
        Refusal::NoMaster { source, .. } => assert_eq!(source, dir.path()),
        other => panic!("an empty checkout answered {other:?}"),
    }
    assert!(
        refusal.to_string().contains("holosmith config edit"),
        "{refusal}"
    );
}

#[test]
fn work_the_commit_does_not_have_makes_the_tree_dirty() {
    let dir = TempDir::new("candidate-dirty");
    repository_on(dir.path(), "master");
    std::fs::write(dir.file("README.md"), "one\n").expect("the file is writable");
    git(dir.path(), &["add", "README.md"]);
    git(dir.path(), &["commit", "--quiet", "-m", "the readme"]);
    // One modified tracked file and one untracked file: both are work `master` does not hold.
    std::fs::write(dir.file("README.md"), "two\n").expect("the file is writable");
    std::fs::write(dir.file("notes.txt"), "notes\n").expect("the file is writable");
    // The repository's own config would hide the untracked file from a bare `status --porcelain`; `-uall` is what
    // makes it visible, so this is the case the flag exists for.
    git(dir.path(), &["config", "status.showUntrackedFiles", "no"]);

    let refusal =
        candidate::resolve(&config(dir.path())).expect_err("a dirty tree is not a commit");
    match &refusal {
        Refusal::Dirty { source, entries } => {
            assert_eq!(source, dir.path());
            assert!(
                entries.iter().any(|entry| entry.contains("README.md")),
                "{entries:?}"
            );
            assert!(
                entries.iter().any(|entry| entry.contains("notes.txt")),
                "{entries:?}"
            );
        }
        other => panic!("a dirty tree answered {other:?}"),
    }
    let sentence = refusal.to_string();
    assert!(
        sentence.contains("README.md") && sentence.contains("notes.txt"),
        "{sentence}"
    );
    assert!(sentence.contains("stash --include-untracked"), "{sentence}");
}

#[test]
fn a_dirty_name_with_a_non_ascii_byte_is_named_rather_than_escaped() {
    let dir = TempDir::new("candidate-dirty-name");
    repository_on(dir.path(), "master");
    std::fs::write(dir.file("uu-ärger.md"), "work\n").expect("the file is writable");

    let refusal = candidate::resolve(&config(dir.path())).expect_err("an untracked file is work");
    let sentence = refusal.to_string();
    assert!(sentence.contains("uu-ärger.md"), "{sentence}");
    assert!(
        !sentence.contains(r"\303"),
        "`-z` is what keeps `core.quotePath` from octal-escaping it: {sentence}"
    );
}

#[test]
fn a_rename_is_two_entries_with_a_status_each() {
    let dir = TempDir::new("candidate-rename");
    repository_on(dir.path(), "master");
    std::fs::write(dir.file("old.txt"), "one\n").expect("the file is writable");
    git(dir.path(), &["add", "old.txt"]);
    git(dir.path(), &["commit", "--quiet", "-m", "the file"]);
    git(dir.path(), &["mv", "old.txt", "new.txt"]);

    let refusal = candidate::resolve(&config(dir.path())).expect_err("a staged rename is work");
    match &refusal {
        Refusal::Dirty { entries, .. } => {
            assert_eq!(entries.len(), 2, "{entries:?}");
            assert!(
                entries.iter().any(|entry| entry.ends_with("new.txt")),
                "{entries:?}"
            );
            assert!(
                entries.iter().any(|entry| entry.ends_with("old.txt")),
                "{entries:?}"
            );
            assert!(
                entries
                    .iter()
                    .all(|entry| entry.starts_with('A') || entry.starts_with('D')),
                "`--no-renames` is what gives every record its own status columns: {entries:?}"
            );
        }
        other => panic!("a staged rename answered {other:?}"),
    }
}
