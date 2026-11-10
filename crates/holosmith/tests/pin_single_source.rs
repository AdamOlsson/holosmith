//! The pinned pi version lives in `pins/pi.txt` and nowhere else (HS-001 criterion 3).

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

/// The digest the builder recipe's rust base is pinned to (HS-076), read out of the recipe itself.
///
/// A builder image's pin is its `FROM` line, the same place the two product recipes keep theirs. What this test
/// adds is the rule `pins/pi.txt` has: the value lives in **one** file, so a second copy — in a doc, a review, or a
/// second Dockerfile — is a failure rather than a value that can drift.
fn builder_base_digest() -> String {
    let line = holosmith::builder::DOCKERFILE
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("FROM rust:"))
        .expect("the builder recipe names a rust base");
    // The `FROM <ref> AS <stage>` line's second word is the reference; the digest is its last `@sha256:` field.
    line.split_whitespace()
        .nth(1)
        .and_then(|reference| reference.rsplit_once("@sha256:"))
        .map(|(_, digest)| digest.to_string())
        .expect("the builder's rust base is pinned by digest")
}

#[test]
fn the_builder_recipes_base_digest_appears_in_one_file() {
    let digest = builder_base_digest();
    assert_eq!(digest.len(), 64, "a digest is 64 hex characters: {digest}");
    let root = repo_root();
    let files = match tracked_files(&root) {
        Some(files) => files,
        None => walk_files(&root),
    };
    assert!(!files.is_empty(), "no files to search under {root:?}");
    let needle = digest.as_bytes();
    let mut offenders: Vec<&str> = files
        .iter()
        .filter(|file| *file != "crates/holosmith/images/builder.Dockerfile")
        .filter(|file| std::fs::read(root.join(file)).is_ok_and(|bytes| contains(&bytes, needle)))
        .map(String::as_str)
        .collect();
    offenders.sort_unstable();
    assert!(
        offenders.is_empty(),
        "the builder image's base digest {digest} also appears in {offenders:?}; it belongs in \
         crates/holosmith/images/builder.Dockerfile only"
    );
}

/// Every file in the worktree except build output and git history, for trees with no
/// `.git` to list tracked files from.
fn walk_files(root: &Path) -> Vec<String> {
    const SKIPPED: [&str; 3] = [".git", "target", "dist"];
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
