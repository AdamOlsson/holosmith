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
