//! The release tree: `releases/<tag>/`, `current`, and the unit's `ExecStart` (spec item 10).
//!
//! Item 10's first paragraph — "releases live in `releases/<tag>/` with a `current` symlink that systemd runs" — is
//! three claims, and each is measured here over a real directory: a release is a binary **and** its digest, one tag
//! holds one binary, the switch is a `rename` of a **relative** symlink, and what systemd executes is the path
//! *through* the link (so a later `server deploy` renders the one unit that follows a promotion).
//!
//! No daemon, no database and no process: this is a module of directories and one symlink, and a test that needed
//! either of the other two would be measuring somebody else's story. a later change owns what the layout means from here
//! (pruning, `doctor`'s report, `version` from the release path); what is asserted below is what a promotion needs.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use holosmith::images;
use holosmith::release::{self, Release, Written};

mod support;
use support::TempDir;

/// The digest of `bytes`, through the product's own file reader: `images::digest` is what `build` writes beside an
/// artifact and what `upgrade` compares a candidate's `.sha256` with.
fn digest_of(dir: &TempDir, bytes: &[u8]) -> String {
    let path = dir.file("fixture-binary");
    std::fs::write(&path, bytes).expect("the fixture binary is writable");
    images::digest(&path).expect("the fixture binary is readable")
}

/// A tree with one release in it: `<root>/opt/holosmith/releases/v1/{holosmith, holosmith.sha256}`.
fn tree(dir: &TempDir) -> Release {
    let release = Release::at(dir.file("opt/holosmith"));
    std::fs::create_dir_all(release.releases()).expect("the releases directory is creatable");
    let bytes = b"#!/bin/sh\nexit 0\n";
    let digest = digest_of(dir, bytes);
    release
        .write("v1", bytes, &digest)
        .expect("a release is writable");
    release
}

#[test]
fn a_release_is_a_binary_and_the_digest_that_names_it() {
    let dir = TempDir::new("release-write");
    let release = tree(&dir);
    let binary = release.binary("v1");
    assert_eq!(
        std::fs::read(&binary).expect("the release's binary is readable"),
        b"#!/bin/sh\nexit 0\n"
    );
    let mode = std::fs::metadata(&binary)
        .expect("the binary's metadata reads")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755, "a release's binary has to be executable");
    let bytes = b"#!/bin/sh\nexit 0\n";
    let digest = digest_of(&dir, bytes);
    assert_eq!(
        std::fs::read_to_string(release.dir_of("v1").join(release::DIGEST))
            .expect("the digest file is readable"),
        format!("{digest}  holosmith\n"),
        "the digest file is not in sha256sum's own format"
    );
    // The same bytes a second time are `Current`: a re-run writes nothing and cannot fail on its own artifact.
    assert_eq!(
        release
            .write("v1", bytes, &digest)
            .expect("the same bytes are accepted"),
        Written::Current
    );
    // A *different* binary under the same tag is refused, with both digests named: one release directory holding two
    // binaries is the ambiguity a rollback cannot resolve.
    let other = b"#!/bin/sh\nexit 1\n";
    let other_digest = digest_of(&dir, other);
    let error = release
        .write("v1", other, &other_digest)
        .expect_err("a second binary under one tag is refused");
    assert!(
        error.contains(&digest) && error.contains(&other_digest) && error.contains("v1"),
        "{error}"
    );
    // And a tag that is not one path segment never reaches a write at all.
    let error = release
        .write("a/b", b"x", &digest_of(&dir, b"x"))
        .expect_err("a tag with a slash is refused");
    assert!(error.contains("a/b"), "{error}");
}

#[test]
fn a_switch_is_a_relative_link_replaced_in_one_step() {
    let dir = TempDir::new("release-switch");
    let release = tree(&dir);
    let bytes = b"#!/bin/sh\nexit 2\n";
    let digest = digest_of(&dir, bytes);
    release
        .write("v2", bytes, &digest)
        .expect("a second release is writable");

    release.switch("v1").expect("the first release is live");
    assert_eq!(
        release.link(),
        Some(Path::new("releases/v1").to_path_buf()),
        "the link target has to be relative, or the tree is not relocatable"
    );
    assert_eq!(
        release.live(),
        Some(release.current().join("holosmith")),
        "systemd runs the path *through* the link, not the directory it resolves to"
    );

    release.switch("v2").expect("the second release is live");
    assert_eq!(release.link(), Some(Path::new("releases/v2").to_path_buf()));
    assert_eq!(
        release.tag_of(&release.link().expect("the switch just wrote a link")),
        Some("v2".to_string())
    );
    // Nothing but the link itself is left beside it: the staged name the switch writes is renamed, not copied.
    let names: Vec<String> = std::fs::read_dir(release.root())
        .expect("the tree reads")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("current"))
        .collect();
    assert_eq!(
        names,
        ["current"],
        "a staged symlink outlived the switch: {names:?}"
    );

    // A rollback restores the target **verbatim**, including one an operator made absolute.
    release
        .restore(Path::new("/elsewhere/releases/v1"))
        .expect("an absolute target is restorable");
    assert_eq!(
        release.link(),
        Some(Path::new("/elsewhere/releases/v1").to_path_buf())
    );
    // ... and that one is not one of this tree's releases, so `live` does not claim it is.
    assert_eq!(release.live(), None);
}

#[test]
fn live_is_none_until_the_link_names_a_binary() {
    let dir = TempDir::new("release-live");
    let release = Release::at(dir.file("opt/holosmith"));
    assert_eq!(release.live(), None, "an empty tree has no live binary");
    std::fs::create_dir_all(release.dir_of("v1")).expect("the release directory is creatable");
    release.switch("v1").expect("the link is writable");
    assert_eq!(
        release.live(),
        None,
        "a link to a directory with no binary is not a live release"
    );
}

#[test]
fn the_unit_names_the_path_through_current_and_a_promotion_can_read_it_back() {
    // The two readers of one unit: `deploy` renders it and `upgrade` reads it back, and they have to agree about the
    // program, or a promotion seeds the previous release from the wrong file.
    let dir = TempDir::new("release-unit");
    let release = tree(&dir);
    release.switch("v1").expect("the release is live");
    let exe = release.live().expect("the tree is live");
    let root = holosmith::deploy::Root::resolve(Some(dir.path().to_path_buf()));
    let unit = holosmith::deploy::unit_text(&exe, holosmith::config::Role::Server, &root);
    assert!(
        unit.contains(&format!("ExecStart={} serve", exe.display())),
        "{unit}"
    );
    assert_eq!(release::unit_executable(&unit), Some(exe));
    // The `ExecStartPre` line is *not* what the reader takes: it is the doctor, not the server.
    assert!(
        unit.contains("ExecStartPre=") && !unit.contains("ExecStartPre=ExecStart"),
        "{unit}"
    );
}

#[test]
fn a_binary_run_from_a_release_directory_reports_that_release() {
    // Acceptance bullet 3, measured over a real file in a real tree: the fact is *the path the file is in*, and a
    // binary started through `current` reports the directory the link resolves to — which is what makes the live
    // version unambiguous.
    let dir = TempDir::new("release-version");
    let release = tree(&dir);
    release.switch("v1").expect("the release is live");
    // The test binary is the release's bytes: a copy of *a* program, which is what a hand-seeded tree holds.
    std::fs::copy(env!("CARGO_BIN_EXE_holosmith"), release.binary("v1"))
        .expect("the test binary is copyable");

    let reported = |exe: &Path| -> String {
        let output = std::process::Command::new(exe)
            .args(["version"])
            .output()
            .expect("the copy runs");
        assert!(output.status.success(), "version exited non-zero");
        String::from_utf8(output.stdout).expect("version prints UTF-8")
    };
    assert!(
        reported(&release.binary("v1")).contains("\nrelease v1\n"),
        "the release directory is the fact: {}",
        reported(&release.binary("v1"))
    );
    // Through the link, `current_exe` resolves the symlink (Linux reads `/proc/self/exe`), so the answer is the
    // release directory and not the word `current`.
    let through = release.live().expect("the tree is live");
    assert!(
        reported(&through).contains("\nrelease v1\n"),
        "a binary started through `current` reports nothing: {}",
        reported(&through)
    );
    // And a copy outside the tree reports no release at all, on the same line.
    let loose = dir.file("holosmith");
    std::fs::copy(env!("CARGO_BIN_EXE_holosmith"), &loose).expect("the test binary is copyable");
    assert!(
        reported(&loose).contains("\nrelease -\n"),
        "a binary outside a release directory reports one: {}",
        reported(&loose)
    );
}
