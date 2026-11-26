//! `scripts/build.sh` as a machine runs it: the four flags, the three checks that come before anything expensive, the
//! toolchain it finds or fetches, and the tree facts the one release target is spelled by.
//!
//! Nothing here compiles the tree or reaches the network. Each test copies the script and the files it reads before
//! it compiles into a temporary tree and runs it with a shim directory first on `PATH`, so every run stops where the
//! shim does: a shim `npm` is how "it got this far and no further" is measured, and the fixture is also what keeps a
//! run's `toolchains/` and `dist/` out of this worktree.

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The tree facts below exclude this file: it is the guard, and the strings it looks for are here.
const GUARD: &str = "crates/holosmith/tests/build_script.rs";

/// What a run prints once it reaches the web build, so a refusal's own stage can be told from a later one's.
const WEB_MARKER: &str = "SHIM-REACHED-WEB";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root above the crate directory exists")
}

/// `bash`, by absolute path: the runs below set `PATH` to a shim directory, and resolving the shell through that
/// would be resolving it through the thing under test.
fn bash() -> PathBuf {
    ["/bin/bash", "/usr/bin/bash"]
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
        .expect("bash is on this machine")
}

/// The rustc `pins/rust.txt` holds, as a version: the line is the base's whole reference, and the version is the part
/// before the first `-`.
fn pinned_rust() -> String {
    let pin =
        fs::read_to_string(repo_root().join("pins/rust.txt")).expect("pins/rust.txt is readable");
    pin.trim().split_once('-').map_or_else(
        || pin.trim().to_string(),
        |(version, _)| version.to_string(),
    )
}

/// A directory that removes itself, so a test leaves nothing behind.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = env::temp_dir().join(format!("holosmith-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temp directory is creatable");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The tree a run needs, and only that: the script itself, the two files it reads before it compiles anything, and an
/// empty `web/` the web build can enter. A fourth read added ahead of the web build fails these tests loudly rather
/// than leaving a fixture that quietly stopped being the tree.
struct Tree {
    dir: TempDir,
}

impl Tree {
    const FILES: [&str; 4] = [
        "scripts/build.sh",
        "scripts/build-web.sh",
        "pins/rust.txt",
        "crates/holosmith/images/agent.Dockerfile",
    ];

    fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        for file in Self::FILES {
            let destination = dir.path().join(file);
            fs::create_dir_all(
                destination
                    .parent()
                    .expect("a file under the tree has a parent"),
            )
            .expect("the destination directory is creatable");
            fs::copy(repo_root().join(file), &destination)
                .unwrap_or_else(|error| panic!("{file} copies into the fixture: {error}"));
        }
        fs::create_dir_all(dir.path().join("web")).expect("web/ is creatable");
        Self { dir }
    }

    fn script(&self) -> PathBuf {
        self.dir.path().join("scripts/build.sh")
    }

    fn dir(&self) -> &Path {
        self.dir.path()
    }
}

/// A shim directory: the one thing a run may reach for a program, so a fact about which program it reached is a fact
/// about the files here.
struct Shims {
    dir: PathBuf,
}

impl Shims {
    fn new(tree: &Tree) -> Self {
        let dir = tree.dir().join("shims");
        fs::create_dir_all(&dir).expect("the shim directory is creatable");
        let shims = Self { dir };
        // The machine's own answer, so a run below is a fact about this fixture rather than about the host the suite
        // happens to run on: the script reads it before the compiler check. One test wants another machine.
        shims.add("uname", "printf 'x86_64\\n'");
        shims
    }

    fn add(&self, name: &str, body: &str) -> &Self {
        let path = self.dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("the shim is writable");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("the shim is executable");
        self
    }

    /// The driver and the compiler, answering the versions they are given: every run below sets both, because the
    /// compiler check is the first thing after the tag.
    fn compiler(&self, cargo: &str, rustc: &str) -> &Self {
        self.add(
            "cargo",
            &format!("printf '%s\\n' 'cargo {cargo} (shim 2026-01-01)'"),
        )
        .add(
            "rustc",
            &format!("printf '%s\\n' 'rustc {rustc} (shim 2026-01-01)'"),
        )
    }

    /// The musl cross linker `PATH` is searched for first. A run that leaves it out is a run whose only remaining
    /// answers are this tree's `toolchains/` and the fetch.
    fn linker(&self) -> &Self {
        self.add("x86_64-linux-musl-gcc", "exit 0")
    }

    /// The web build, which is where a run that got past the checks stops: `npm` prints the marker and fails, so the
    /// marker's presence is the measurement and the exit status is not.
    fn web(&self) -> &Self {
        self.add("node", "exit 0")
            .add("npm", &format!("printf '%s\\n' '{WEB_MARKER}'; exit 1"))
    }

    /// The fetch, as a program that records its argv and writes bytes the pin will refuse. `-o <path>` is where a
    /// real `curl` puts the tarball.
    fn fetcher(&self) -> &Self {
        self.add(
            "curl",
            "log=\"${FETCH_LOG:-/dev/null}\"\n\
             printf '%s\\n' \"$*\" >>\"$log\"\n\
             while [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2 ;; *) shift ;; esac; done\n\
             printf 'not the pinned bytes\\n' >\"$out\"",
        )
    }

    /// `PATH` for a run: the shims, then the programs a shell script is written against.
    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.dir.display())
    }
}

fn run(script: &Path, args: &[&str], path: &str, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(bash());
    command.arg(script).args(args).env("PATH", path);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("bash runs the script")
}

/// stdout and stderr as one text: a refusal may be printed on either, and a test about *which* sentence was printed
/// is a test of one run rather than of the two streams.
fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn code(output: &Output) -> i32 {
    output
        .status
        .code()
        .expect("the run exited rather than died of a signal")
}

#[test]
fn help_prints_the_four_flags_and_nothing_internal() {
    // `--help` is the script's header comment, so this is what a reader gets before they have a tree, a toolchain or a
    // reason to trust either. Every flag it prints is one it parses, and nothing in it may name a decision record, a
    // ticket or a machine's own directory: those are the reader's second question, not the script's usage line.
    let output = run(
        &repo_root().join("scripts/build.sh"),
        &["--help"],
        "/usr/bin:/bin",
        &[],
    );
    assert_eq!(code(&output), 0, "{}", combined(&output));
    let text = combined(&output);
    for flag in [
        "--profile dev|release",
        "--target",
        "--no-check",
        "--require-clean-tree",
    ] {
        assert!(
            text.contains(flag),
            "`--help` does not print {flag}: {text}"
        );
    }
    for internal in ["ADR-", "HOL-", "PER-", "~/.pi"] {
        assert!(
            !text.contains(internal),
            "`--help` names {internal}, which a reader outside this tree cannot resolve: {text}"
        );
    }
}

#[test]
fn a_target_that_is_not_supported_is_refused_before_anything_is_fetched() {
    // The one release target is a fact the script states rather than one a reader discovers from a missing linker, so
    // the refusal has to happen before the toolchain work — which is also what makes it measurable with no network.
    let tree = Tree::new("build-script-unsupported");
    let shims = Shims::new(&tree);
    let log = tree.dir().join("fetch.log");
    let log_env = log.display().to_string();
    let output = run(
        &tree.script(),
        &["--target", "aarch64-unknown-linux-musl"],
        &shims.path(),
        &[("FETCH_LOG", log_env.as_str())],
    );
    assert_eq!(code(&output), 2, "{}", combined(&output));
    let text = combined(&output);
    assert!(
        text.contains("aarch64-unknown-linux-musl")
            && text.contains("not a supported release target"),
        "the refusal does not name the triple and the rule: {text}"
    );
    assert!(
        !log.exists(),
        "an unsupported target fetched {} anyway",
        log.display()
    );
}

#[test]
fn a_machine_that_is_not_x86_64_is_refused_before_the_build_asks_anything() {
    // One release target is also one architecture: an aarch64 host would cross-build the artifact and then fail to run
    // it, so the boundary is a sentence here rather than the shell's exec error out of `scripts/check-static.sh` after a
    // full build.
    let tree = Tree::new("build-script-machine");
    let shims = Shims::new(&tree);
    let pinned = pinned_rust();
    shims.compiler(&pinned, &pinned).linker().web().fetcher();
    shims.add("uname", "printf 'aarch64\\n'");
    let output = run(&tree.script(), &["--no-check"], &shims.path(), &[]);
    assert_eq!(code(&output), 1, "{}", combined(&output));
    let text = combined(&output);
    assert!(
        text.contains("this machine answers aarch64")
            && text.contains("x86_64-unknown-linux-musl only"),
        "the refusal names neither the machine's own answer nor the one target this build makes: {text}"
    );
    assert!(
        !text.contains(WEB_MARKER),
        "a machine this build cannot run on reached the web build: {text}"
    );
}

#[test]
fn a_repeated_target_is_built_once() {
    // `--target` is a set and not a sequence: it stays repeatable because a second architecture returns through it,
    // and a caller that names the same triple twice must not pay for two builds of it.
    let tree = Tree::new("build-script-repeat");
    let shims = Shims::new(&tree);
    let pinned = pinned_rust();
    shims.compiler(&pinned, &pinned).linker().web().fetcher();
    let target = "x86_64-unknown-linux-musl";
    let output = run(
        &tree.script(),
        &["--no-check", "--target", target, "--target", target],
        &shims.path(),
        &[],
    );
    let text = combined(&output);
    let announced: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("targets: "))
        .collect();
    assert_eq!(
        announced,
        [format!("targets: {target}")],
        "a repeated target is not one target: {text}"
    );
}

#[test]
fn a_dirty_tree_is_refused_before_the_build_asks_anything() {
    let tree = Tree::new("build-script-dirty");
    let shims = Shims::new(&tree);
    let pinned = pinned_rust();
    shims.compiler(&pinned, &pinned).linker().web().fetcher();
    let output = run(
        &tree.script(),
        &["--require-clean-tree"],
        &shims.path(),
        &[("HOLOSMITH_GIT_TAG", "abc1234-dirty")],
    );
    assert_eq!(code(&output), 1, "{}", combined(&output));
    let text = combined(&output);
    assert!(
        text.contains("abc1234-dirty") && text.contains("not a release"),
        "the refusal does not name the tag it would have reported: {text}"
    );
    assert!(
        !text.contains(WEB_MARKER),
        "a dirty tree reached the web build before it was refused: {text}"
    );
}

#[test]
fn a_clean_tree_gets_past_the_tag_check() {
    // The other half of `--require-clean-tree`, and the shim `npm` is what says so: the run has to reach the next
    // stage and stop there, rather than pass because nothing ran.
    let tree = Tree::new("build-script-clean");
    let shims = Shims::new(&tree);
    let pinned = pinned_rust();
    shims.compiler(&pinned, &pinned).linker().web().fetcher();
    let output = run(
        &tree.script(),
        &["--require-clean-tree"],
        &shims.path(),
        &[("HOLOSMITH_GIT_TAG", "abc1234")],
    );
    let text = combined(&output);
    assert_eq!(
        code(&output),
        1,
        "the shim npm should have failed the run: {text}"
    );
    assert!(
        text.contains(WEB_MARKER),
        "a clean tree did not reach the web build: {text}"
    );
    assert!(
        !text.contains("-dirty"),
        "a clean tag was reported as dirty: {text}"
    );
}

#[test]
fn a_cargo_of_another_version_is_refused_by_name() {
    let tree = Tree::new("build-script-cargo");
    let shims = Shims::new(&tree);
    let pinned = pinned_rust();
    shims.compiler("1.100.0", &pinned).web().fetcher();
    let output = run(&tree.script(), &["--no-check"], &shims.path(), &[]);
    assert_eq!(code(&output), 1, "{}", combined(&output));
    let text = combined(&output);
    assert!(
        text.contains("1.100.0") && text.contains(&pinned),
        "the refusal names neither the compiler found nor the pinned one: {text}"
    );
    assert!(
        !text.contains(WEB_MARKER),
        "a compiler of the wrong version reached the web build: {text}"
    );
}

#[test]
fn a_rustc_of_another_version_is_refused_by_name() {
    // The driver and the compiler are two answers, and the artifact is the pin's only when both are.
    let tree = Tree::new("build-script-rustc");
    let shims = Shims::new(&tree);
    let pinned = pinned_rust();
    shims.compiler(&pinned, "1.100.0").web().fetcher();
    let output = run(&tree.script(), &["--no-check"], &shims.path(), &[]);
    assert_eq!(code(&output), 1, "{}", combined(&output));
    let text = combined(&output);
    assert!(
        text.contains("rustc --version answers 1.100.0") && text.contains(&pinned),
        "the refusal does not name the rustc found and the pinned one: {text}"
    );
}

#[test]
fn a_linker_on_path_is_used_and_nothing_is_fetched() {
    // `PATH` first is load-bearing rather than tidy: a session's container carries the linker and has no network, so
    // the fetch must never be reached there.
    let tree = Tree::new("build-script-linker");
    let shims = Shims::new(&tree);
    let pinned = pinned_rust();
    shims.compiler(&pinned, &pinned).linker().web().fetcher();
    let log = tree.dir().join("fetch.log");
    let log_env = log.display().to_string();
    let output = run(
        &tree.script(),
        &["--no-check"],
        &shims.path(),
        &[("FETCH_LOG", log_env.as_str())],
    );
    let text = combined(&output);
    assert!(
        text.contains("linker: ") && text.contains("x86_64-linux-musl-gcc"),
        "the run did not resolve the linker it was given: {text}"
    );
    assert!(
        !log.exists(),
        "a linker on PATH did not stop the fetch: {}",
        fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(
        !tree.dir().join("toolchains").exists(),
        "a linker on PATH still created toolchains/"
    );
}

#[test]
fn no_linker_and_no_toolchains_fetches_and_refuses_bytes_that_do_not_match() {
    // The fetch is the third answer and the last one: with no linker anywhere, the pinned URL is read out of the
    // recipe, the bytes are fetched, and a digest that does not match is a refusal that extracts nothing.
    let tree = Tree::new("build-script-fetch");
    let shims = Shims::new(&tree);
    let pinned = pinned_rust();
    shims.compiler(&pinned, &pinned).web().fetcher();
    let log = tree.dir().join("fetch.log");
    let log_env = log.display().to_string();
    let output = run(
        &tree.script(),
        &["--no-check"],
        &shims.path(),
        &[("FETCH_LOG", log_env.as_str())],
    );
    assert_eq!(code(&output), 1, "{}", combined(&output));
    let text = combined(&output);
    assert!(
        text.contains("do not match the digest"),
        "the refusal is not the digest's: {text}"
    );
    let fetched = fs::read_to_string(&log).expect("the fetch was asked");
    assert!(
        fetched.contains("https://musl.cc/x86_64-linux-musl-cross.tgz"),
        "the fetch did not name the pinned URL: {fetched}"
    );
    assert!(
        !tree
            .dir()
            .join("toolchains/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc")
            .exists(),
        "a tarball that failed its digest was extracted anyway"
    );
    assert!(
        !tree
            .dir()
            .join("toolchains/x86_64-linux-musl-cross.tgz")
            .exists(),
        "the tarball that failed its digest was left behind"
    );
    assert!(
        !text.contains(WEB_MARKER),
        "a failed fetch reached the web build: {text}"
    );
}

#[test]
fn the_pinned_tarball_and_its_digest_live_in_the_recipe_only() {
    // One copy of the digest in the tree, and the script reads it out of the recipe rather than carrying a second one:
    // a literal here is the copy nobody edits when the pin moves.
    let script = fs::read_to_string(repo_root().join("scripts/build.sh"))
        .expect("scripts/build.sh is readable");
    let literals = hex_runs(&script, 64);
    assert!(
        literals.is_empty(),
        "scripts/build.sh holds a 64-character hex digest, which belongs in the recipe: {literals:?}"
    );
    let recipe = fs::read_to_string(repo_root().join("crates/holosmith/images/agent.Dockerfile"))
        .expect("agent.Dockerfile is readable");
    assert!(
        recipe.contains("https://musl.cc/x86_64-linux-musl-cross.tgz"),
        "the recipe no longer fetches the tarball the build fetches"
    );
    assert!(
        !hex_runs(&recipe, 64).is_empty(),
        "the recipe pins no digest, so the build's fetch would check nothing"
    );
}

#[test]
fn the_cargo_config_names_one_target_and_it_is_the_supported_one() {
    let text = fs::read_to_string(repo_root().join(".cargo/config.toml"))
        .expect(".cargo/config.toml is readable");
    let targets: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("[target."))
        .collect();
    assert_eq!(
        targets,
        ["[target.x86_64-unknown-linux-musl]"],
        "the build's cargo config does not name exactly the one release target"
    );
    assert!(
        text.contains("relocation-model=static"),
        "the static relocation the artifacts are proved by is gone: {text}"
    );
    assert!(
        text.contains("RUSTFLAGS"),
        "the warning against exporting RUSTFLAGS left with the second target, and it protects the first"
    );
}

#[test]
fn no_tracked_file_outside_the_decisions_names_the_emulator() {
    // The emulator was only ever for an artifact this machine is not: with one release target there is nothing to run
    // under one, and a leftover `command -v qemu…` is a `SKIP` waiting for a machine that has it.
    let root = repo_root();
    let files = tracked_or_walked(&root);
    assert!(!files.is_empty(), "no files to search under {root:?}");
    let mut offenders: Vec<&str> = files
        .iter()
        .filter(|file| *file != GUARD && !file.starts_with("docs/adr/"))
        .filter(|file| fs::read(root.join(file)).is_ok_and(|bytes| contains(&bytes, b"qemu")))
        .map(String::as_str)
        .collect();
    offenders.sort_unstable();
    assert!(
        offenders.is_empty(),
        "an emulator is still named in {offenders:?}; the decisions under docs/adr/ are the only place it belongs"
    );
}

#[test]
fn nothing_that_builds_the_artifact_names_the_second_triple() {
    // `scripts/build.sh`, `scripts/check-static.sh`, the cargo config, the agent recipe and the prose a reader starts
    // from all speak of one release target now. `scripts/verify.sh` is not in the set: its `case_build` case drives
    // `holosmith build`, whose own target list still names both until that verb goes, and a hand-built aarch64
    // artifact is still something a deploy can be handed.
    let root = repo_root();
    let files = tracked_or_walked(&root);
    assert!(!files.is_empty(), "no files to search under {root:?}");
    let mut offenders: Vec<&str> = files
        .iter()
        .filter(|file| names_the_second_target(file.as_str()))
        .filter(|file| {
            fs::read(root.join(file))
                .is_ok_and(|bytes| contains(&bytes, b"aarch64-unknown-linux-musl"))
        })
        .map(String::as_str)
        .collect();
    offenders.sort_unstable();
    assert!(
        offenders.is_empty(),
        "the second release target is still named in {offenders:?}"
    );
}

/// Whether a file is one of the ones that build the artifact or describe what it is.
fn names_the_second_target(file: &str) -> bool {
    if file == GUARD || file.starts_with("docs/adr/") || file == "scripts/verify.sh" {
        return false;
    }
    file.starts_with("scripts/")
        || file.starts_with("docs/")
        || file == "AGENTS.md"
        || file == "README.md"
        || file == ".cargo/config.toml"
        || file == "crates/holosmith/images/agent.Dockerfile"
}

/// The runs of hex characters of exactly `width` in `text`.
fn hex_runs(text: &str, width: usize) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_hexdigit() {
            at += 1;
            continue;
        }
        let mut end = at;
        while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
            end += 1;
        }
        if end - at == width {
            found.push(text[at..end].to_string());
        }
        at = end;
    }
    found
}

fn tracked_or_walked(root: &Path) -> Vec<String> {
    tracked_files(root).unwrap_or_else(|| walk_files(root))
}

/// Every file in the worktree except build output and git history, for trees with no `.git` to list tracked files
/// from — the shape `tests/pin_single_source.rs` uses, so a builder container measures the same rules.
fn walk_files(root: &Path) -> Vec<String> {
    const SKIPPED: [&str; 5] = [".git", "target", "dist", "node_modules", "toolchains"];
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
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
