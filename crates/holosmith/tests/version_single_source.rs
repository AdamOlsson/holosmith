//! The version this binary reports lives in `VERSION` at the repository root and nowhere else.
//!
//! Three files could otherwise answer the same question: `VERSION` (the truth), `Cargo.toml`'s
//! `[workspace.package] version` (which Cargo requires, and which the build reads as a placeholder), and the web
//! app's `package.json` (which npm requires). The rule is that the two manifests stay frozen at the placeholder, so
//! that **a bump edits one file** — and, more than tidiness, so that a bump stays out of `Cargo.lock`.
//!
//! That last part is the load-bearing half. `scripts/check-crate-set.sh` digests `Cargo.lock` and compares it with
//! the crate set the agent image vendored, and a version living in the lock moves that digest while the crate set
//! stands still: the gate then answers 3, a promotion refuses, and the tree's only real change was a number. So the
//! placeholder is a promotion's precondition, and this file is what keeps it a placeholder.
//!
//! The last test forbids the second *reader* as well as the second copy: the cargo package version compiles, and
//! answers `0.0.0` — which is the shape of a bug nobody notices until a release reports a version nothing agrees
//! with. `holosmith::VERSION` is how code reads it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The two manifests' frozen version: `CARGO_PKG_VERSION`'s value, and never this tree's version.
const PLACEHOLDER: &str = "0.0.0";

/// The one file allowed to spell the read, because it is the guard that forbids it.
const GUARD: &str = "crates/holosmith/tests/version_single_source.rs";

/// What the guard looks for: the *read* of the cargo package version, not the identifier in prose. A doc comment
/// naming the variable is a reader being told what not to do; `env!("…")` is a reader doing it.
const READ: &[u8] = b"env!(\"CARGO_PKG_VERSION\")";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root above the crate directory exists")
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()))
}

#[test]
fn the_version_file_is_one_line_and_is_what_the_binary_reports() {
    let contents = read("VERSION");
    assert_eq!(
        contents.lines().count(),
        1,
        "VERSION must hold one version on one line, got {contents:?}"
    );
    assert_eq!(contents.trim(), holosmith::VERSION);
    assert_eq!(
        holosmith::VERSION.split_whitespace().count(),
        1,
        "a version with a space in it would reach an image tag and `version --json`: {:?}",
        holosmith::VERSION
    );
}

#[test]
fn both_manifests_hold_the_placeholder_and_not_the_version() {
    let manifest = read("Cargo.toml");
    let declared = workspace_version(&manifest);
    assert_eq!(
        declared, PLACEHOLDER,
        "Cargo.toml's [workspace.package] version is {declared}, and it must stay the {PLACEHOLDER} placeholder: the \
         version is VERSION's, and a real one here would land in Cargo.lock and make every bump read as a stale \
         agent image"
    );
    assert_ne!(
        holosmith::VERSION,
        PLACEHOLDER,
        "VERSION holds the placeholder itself, so a release would report {PLACEHOLDER}"
    );

    let package = read("web/package.json");
    assert!(
        package.contains(&format!("\"version\": \"{PLACEHOLDER}\"")),
        "web/package.json's version must stay the {PLACEHOLDER} placeholder: nothing reads it, and a real one there \
         is a second copy of a number VERSION owns"
    );
}

#[test]
fn the_lock_names_the_placeholder_so_a_bump_never_moves_it() {
    // The whole point, stated where it can fail: `scripts/check-crate-set.sh` digests this file, so the workspace
    // member's own version in it would move the digest on a bump and trip the gate for a tree whose crate set never
    // changed.
    let lock = read("Cargo.lock");
    let member = lock
        .split("[[package]]")
        .find(|block| block.contains("name = \"holosmith\""))
        .expect("Cargo.lock holds a package block for the workspace member");
    assert!(
        member.contains(&format!("version = \"{PLACEHOLDER}\"")),
        "Cargo.lock's holosmith block must name {PLACEHOLDER}, and reads {member:?}: a real version there is what \
         made a bump refuse its own promotion"
    );
}

#[test]
fn no_tracked_source_reads_the_cargo_package_version() {
    let root = repo_root();
    let files = match tracked_files(&root) {
        Some(files) => files,
        // A container that runs this guard can hold the source without its history, where `git ls-files` answers
        // nothing. Walking the worktree there keeps the guard live instead of silently skipping it.
        None => walk_files(&root),
    };
    assert!(!files.is_empty(), "no files to search under {root:?}");

    let mut offenders: Vec<&str> = files
        .iter()
        .filter(|file| *file != GUARD)
        .filter(|file| file.ends_with(".rs"))
        .filter(|file| std::fs::read(root.join(file)).is_ok_and(|bytes| contains(&bytes, READ)))
        .map(String::as_str)
        .collect();
    offenders.sort_unstable();
    assert!(
        offenders.is_empty(),
        "{offenders:?} read env!(\"CARGO_PKG_VERSION\"), which answers Cargo.toml's {PLACEHOLDER} placeholder; \
         holosmith::VERSION is the one reader of the version this build reports"
    );
}

/// The workspace's declared version, out of the one table that may carry one.
///
/// Bounded to that table: `[workspace.dependencies]` carries `version = ` inside its inline tables too, and a scan
/// that ran on would answer a dependency's version for the workspace's.
fn workspace_version(manifest: &str) -> String {
    manifest
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "[workspace.package]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .find_map(|line| line.strip_prefix("version = "))
        .map(|rest| rest.trim().trim_matches('"').to_string())
        .expect("Cargo.toml's [workspace.package] table names a version")
}

/// Every file in the worktree except build output and git history, for trees with no `.git` to list tracked files
/// from.
fn walk_files(root: &Path) -> Vec<String> {
    // The same four `scripts/verify.sh`'s toolchain case leaves out of its copy: none holds a tracked file, and
    // `web/node_modules` holds packages whose sources are not this tree's.
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
