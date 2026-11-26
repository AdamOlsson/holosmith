//! The pinned pi version lives in `pins/pi.txt` and nowhere else, and so does the rust base's reference:
//! `pins/rust.txt` holds it, the recipes take it as `HOLOSMITH_RUST_IMAGE`, and no recipe holds a digest.

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
        // A container that runs this guard can hold the source without its history, where `git
        // ls-files` answers nothing. Walking the worktree there keeps the guard live instead of
        // silently skipping it.
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

/// The recipes that declare a rust stage, listed so that the rule below is about *each of them* rather than
/// about "the one".
///
/// A recipe leaving or joining this list is one line, and the test that the list is complete against the directory is
/// what stops a forgotten entry from making the rules measure nothing.
const RUST_RECIPES: &[&str] = &["crates/holosmith/images/agent.Dockerfile"];

/// The rust base's reference, as `pins/rust.txt` holds it: the tag and the digest, one value.
fn rust_pin() -> &'static str {
    holosmith::pins::rust_image()
}

/// The recipes `crates/holosmith/images/` holds that declare a rust stage, by their file name.
fn recipes_with_a_rust_stage(root: &Path) -> Vec<String> {
    let directory = root.join("crates/holosmith/images");
    let mut found: Vec<String> = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{} is not readable: {error}", directory.display()))
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".Dockerfile"))
        .filter(|entry| {
            std::fs::read_to_string(entry.path())
                .is_ok_and(|text| text.lines().any(|line| line.starts_with("FROM rust:")))
        })
        .map(|entry| {
            format!(
                "crates/holosmith/images/{}",
                entry.file_name().to_string_lossy()
            )
        })
        .collect();
    found.sort();
    found
}

#[test]
fn the_rust_pin_is_one_line_holding_the_whole_reference() {
    let contents = std::fs::read_to_string(repo_root().join("pins/rust.txt"))
        .expect("pins/rust.txt is readable");
    assert_eq!(rust_pin(), contents.trim());
    assert_eq!(
        contents.lines().count(),
        1,
        "pins/rust.txt must hold one reference on one line, got {contents:?}"
    );
    // The whole reference and not the version: Docker resolves `rust:tag@sha256:…` by the **digest** and ignores the
    // tag, so a pin holding only a version could build from one image while claiming another, and nothing would
    // notice. One value in one file cannot drift from itself (ADR-0118).
    let (reference, digest) = rust_pin().rsplit_once("@sha256:").unwrap_or_else(|| {
        panic!("pins/rust.txt is not a reference pinned by digest: {contents:?}")
    });
    assert_eq!(digest.len(), 64, "a digest is 64 hex characters: {digest}");
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "a digest is 64 lowercase hex characters: {digest}"
    );
    // And the tag half is shaped like a version and a Debian suite. Which suite it is is the base's business rather
    // than this test's: nothing in this tree depends on it, and the digest is what pins the bytes.
    let (version, suite) = reference
        .split_once('-')
        .unwrap_or_else(|| panic!("the reference has no Debian suite: {reference}"));
    assert!(
        !suite.is_empty(),
        "the reference names no Debian suite: {reference}"
    );
    let fields: Vec<&str> = version.split('.').collect();
    assert_eq!(
        fields.len(),
        3,
        "the pinned version is not major.minor.patch: {version}"
    );
    assert!(
        fields
            .iter()
            .all(|field| !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit())),
        "the pinned version is not numeric: {version}"
    );
}

#[test]
fn no_tracked_file_beside_the_pin_holds_the_rust_base_digest() {
    // The digest appears in **one** file: a second copy in a recipe, a doc or a review is the stale one nobody
    // edits. Every tracked file is scanned, so a third copy is caught wherever it lands — and the scan does not
    // name the recipes, which is why deleting one needs no edit here.
    let (_, digest) = rust_pin()
        .rsplit_once("@sha256:")
        .expect("the rust base is pinned by digest, not by tag");
    let root = repo_root();
    let files = match tracked_files(&root) {
        Some(files) => files,
        // A container that runs this guard can hold the source without its history, where `git ls-files`
        // answers nothing. Walking the worktree there keeps the guard live instead of silently skipping it.
        None => walk_files(&root),
    };
    assert!(!files.is_empty(), "no files to search under {root:?}");
    let needle = digest.as_bytes();
    let mut offenders: Vec<&str> = files
        .iter()
        .filter(|file| *file != "pins/rust.txt")
        .filter(|file| std::fs::read(root.join(file)).is_ok_and(|bytes| contains(&bytes, needle)))
        .map(String::as_str)
        .collect();
    offenders.sort_unstable();
    assert!(
        offenders.is_empty(),
        "the rust base digest {digest} also appears in {offenders:?}; it belongs in pins/rust.txt only"
    );
}

#[test]
fn every_recipe_with_a_rust_stage_takes_the_reference_from_the_pin() {
    // `RUST_RECIPES` is the list the rule reads, and this assertion is what keeps the list complete: a recipe with a
    // rust stage that is not in it would be pinned by a literal the scan above then has to allow — which is the
    // drift the digest rule exists to catch. So the list is compared with what the directory holds, in both
    // directions.
    let root = repo_root();
    let mut declared: Vec<String> = RUST_RECIPES
        .iter()
        .map(|recipe| recipe.to_string())
        .collect();
    declared.sort();
    assert_eq!(
        declared,
        recipes_with_a_rust_stage(&root),
        "the recipes with a rust stage and the list this rule reads are not the same set"
    );

    for &recipe in RUST_RECIPES {
        let text = std::fs::read_to_string(root.join(recipe))
            .unwrap_or_else(|error| panic!("{recipe} is not readable: {error}"));
        // One spelling for the stage and one for the argument, both `images::RUST_IMAGE_ARG`'s: the argument's name
        // is what `images::build_arguments` passes, and a recipe that spelled it differently would be handed a value
        // it never reads.
        let stage = format!(
            "FROM rust:${{{}}} AS rust",
            holosmith::images::RUST_IMAGE_ARG
        );
        let declared_at = text
            .lines()
            .position(|line| line.trim() == stage)
            .unwrap_or_else(|| {
                panic!("{recipe} does not declare its rust stage as `{stage}`: a literal `FROM rust:` line is the second copy of the pin ADR-0118 removed")
            });
        let argument = format!("ARG {}", holosmith::images::RUST_IMAGE_ARG);
        let argument_at = text
            .lines()
            .position(|line| line.trim() == argument)
            .unwrap_or_else(|| panic!("{recipe} writes `{stage}` and declares no `{argument}`"));
        assert!(
            argument_at < declared_at,
            "{argument} is declared below the first `FROM` in {recipe}, where no `FROM` can read it"
        );
        // Before the *first* `FROM` and in global scope, which is the only scope a `FROM` reads: a `FROM` after an
        // earlier stage would need the argument redeclared there, and this is the assertion that says so.
        let first_from = text
            .lines()
            .position(|line| line.trim_start().starts_with("FROM "))
            .unwrap_or_default();
        assert_eq!(
            argument_at + 1,
            first_from,
            "the rust base's argument in {recipe} is not the instruction immediately before the first `FROM`"
        );
    }
}

#[test]
fn the_manifests_rust_floor_is_the_pins_major_minor() {
    // `Cargo.toml`'s `rust-version` and the pin are **different facts**: a floor cargo refuses to build below, which
    // says nothing about above, and the version an image actually holds. They are tied by major.minor so the floor
    // cannot drift from the pin — a pin moved to another minor is a manifest edit in the same commit, while a patch
    // bump in the pin needs none.
    let manifest =
        std::fs::read_to_string(repo_root().join("Cargo.toml")).expect("Cargo.toml is readable");
    let floor = manifest
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("rust-version"))
        .and_then(|rest| rest.split_once('='))
        .map(|(_, value)| value.trim().trim_matches('"').to_string())
        .expect("Cargo.toml declares a rust-version floor");
    let version = rust_pin()
        .split_once('-')
        .map_or(rust_pin(), |(version, _)| version);
    let major_minor: Vec<&str> = version.split('.').take(2).collect();
    assert_eq!(
        major_minor.join("."),
        floor,
        "Cargo.toml's rust-version floor and the pinned rust base's version have drifted apart"
    );
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
