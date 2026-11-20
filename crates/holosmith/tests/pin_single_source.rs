//! The pinned pi version lives in `pins/pi.txt` and nowhere else, and the two build recipes name one rust base.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The version the binary was compiled with, read from `pins/pi.txt`.
fn pin() -> &'static str {
    holosmith::pins::pi_version()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root above the crate directory exists")
}

#[test]
fn the_embedded_pin_is_the_content_of_pins_pi_txt() {
    let contents =
        std::fs::read_to_string(repo_root().join("pins/pi.txt")).expect("pins/pi.txt is readable");
    assert_eq!(pin(), contents.trim());
    assert_eq!(
        contents.lines().count(),
        1,
        "pins/pi.txt must hold one version on one line, got {contents:?}"
    );
}

#[test]
fn no_other_tracked_file_contains_the_pin() {
    assert!(
        !pin().is_empty(),
        "pins/pi.txt is empty: there is no pin to search for"
    );
    let root = repo_root();
    let files = match tracked_files(&root) {
        Some(files) => files,
        // Builder containers copy the source without history. Walking the worktree there
        // keeps the guard live instead of silently skipping it.
        None => walk_files(&root),
    };
    assert!(!files.is_empty(), "no files to search under {root:?}");

    let needle = pin().as_bytes();
    let mut offenders: Vec<&str> = files
        .iter()
        .filter(|file| *file != "pins/pi.txt" && *file != "Cargo.lock")
        .filter(|file| std::fs::read(root.join(file)).is_ok_and(|bytes| contains(&bytes, needle)))
        .map(String::as_str)
        .collect();
    offenders.sort_unstable();
    assert!(
        offenders.is_empty(),
        "the pinned pi version {} also appears in {offenders:?}; it belongs in pins/pi.txt only",
        pin()
    );
}

/// The rust base a recipe is built from, read out of the recipe itself.
///
/// An image's pin is its `FROM` line, the same place the two product recipes keep theirs, and this one is a
/// **second** such line: `agent.Dockerfile` copies its rust toolchain out of `builder.Dockerfile`'s base, so the two
/// must name the identical reference or a session compiles with a different compiler than a release build.
///
/// The reference is a literal in both files because **a Dockerfile cannot `include` anything**: there is no
/// mechanism that would let one recipe name another's stage or share a fragment, and a build argument would move
/// the pin into the Rust that passes it — which is the second copy this one-file rule exists to forbid.
/// `pins/pi.txt` is what an `include` would look like here, and it is not available to a `FROM` line, so the drift
/// is caught by this test instead.
fn rust_base_reference(dockerfile: &str) -> String {
    let line = dockerfile
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("FROM rust:"))
        .expect("the recipe names a rust base");
    // The `FROM <ref> AS <stage>` line's second word is the reference.
    line.split_whitespace()
        .nth(1)
        .map(str::to_string)
        .expect("the recipe's rust line carries a reference")
}

#[test]
fn the_two_recipes_name_one_rust_base_and_no_third_file_does() {
    // Both halves of one rule: the two references are the same string (so a bump is two edits that cannot drift),
    // and that string's digest appears in **exactly** those two files — a third copy in a doc or a review is the
    // stale one nobody edits.
    let agent = rust_base_reference(holosmith::images::AGENT_DOCKERFILE);
    let builder = rust_base_reference(holosmith::builder::DOCKERFILE);
    assert_eq!(
        agent, builder,
        "the agent image and the builder must be built from one rust base: a session would otherwise compile with a \
         different compiler than a release build"
    );
    let digest = agent
        .rsplit_once("@sha256:")
        .map(|(_, digest)| digest)
        .expect("the rust base is pinned by digest, not by tag");
    assert_eq!(digest.len(), 64, "a digest is 64 hex characters: {digest}");

    let root = repo_root();
    let files = match tracked_files(&root) {
        Some(files) => files,
        None => walk_files(&root),
    };
    assert!(!files.is_empty(), "no files to search under {root:?}");
    let recipes = [
        "crates/holosmith/images/agent.Dockerfile",
        "crates/holosmith/images/builder.Dockerfile",
    ];
    let needle = digest.as_bytes();
    let mut offenders: Vec<&str> = files
        .iter()
        .filter(|file| !recipes.contains(&file.as_str()))
        .filter(|file| std::fs::read(root.join(file)).is_ok_and(|bytes| contains(&bytes, needle)))
        .map(String::as_str)
        .collect();
    offenders.sort_unstable();
    assert!(
        offenders.is_empty(),
        "the rust base digest {digest} also appears in {offenders:?}; it belongs in {recipes:?} only"
    );
    // And the two files the rule names are the two the test read, so a rename cannot leave it searching for a file
    // that is not there.
    for recipe in recipes {
        assert!(
            files.iter().any(|file| file == recipe),
            "{recipe} is not a tracked file, so the search above proved nothing about it"
        );
        assert!(root.join(recipe).is_file(), "{recipe} does not exist");
    }
}

/// Every file in the worktree except build output and git history, for trees with no
/// `.git` to list tracked files from.
fn walk_files(root: &Path) -> Vec<String> {
    // Build output and installed packages, the same ones `scripts/verify.sh`'s toolchain case leaves out of
    // its copy: none holds a tracked file, and `web/node_modules` holds a package's version string that is
    // not this tree's pin.
    const SKIPPED: [&str; 4] = [".git", "target", "dist", "node_modules"];
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if SKIPPED.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file()
                && let Ok(relative) = path.strip_prefix(root)
            {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files
}

fn tracked_files(root: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listed = String::from_utf8(output.stdout).ok()?;
    Some(
        listed
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
