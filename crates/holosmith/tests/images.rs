//! The two embedded Dockerfiles, the label read and the build's own shapes (HS-048, ADR-0057).
//!
//! Two halves, and the split is the design:
//!
//! - **The shapes are asserted without a daemon.** The Dockerfiles are compared with the files under
//!   `crates/holosmith/images/` (so moving one breaks this test rather than a release), the archive is parsed as a
//!   ustar and read back with the `tar` program every machine this repo builds on has, the label comparison is
//!   driven over four arms, and the failure sentence is fed the output this machine's daemon printed for a broken
//!   `RUN` and a broken `COPY`.
//! - **The build itself is measured against a real daemon**, because "the Dockerfile is a recipe that works" is a
//!   claim about Docker rather than about this crate. That half skips — one printed line, no failure — when no
//!   daemon answers or when there is no static artifact to put in the image, because a machine without Docker is a
//!   supported machine and `cargo test` must stay green there.
//!
//! The daemon-gated half builds into **test-only tags** (`holosmith-proxy:test-images`), never the production name:
//! a test that overwrote `holosmith-proxy:<version>` would change what a real deploy's step 3 reads, and a
//! `cargo test` that quietly rebuilt a deployed machine's image would be the kind of side effect nobody looks for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use holosmith::config::Config;
use holosmith::docker::Docker;
use holosmith::images::{self, Expectation, Kind, Log, Status};

mod support;
use support::TempDir;

/// What this machine's daemon printed for a `RUN` that fails, captured from Docker 29.9.2 and trimmed to the
/// lines the sentence is parsed from.
const RUN_FAILURE: &str = "#5 [2/2] RUN apk add --no-cache not-a-package-at-all
#5 1.469 ERROR: unable to select packages:
#5 1.469   not-a-package-at-all (no such package):
#5 1.469     required by: world[not-a-package-at-all]
#5 ERROR: process \"/bin/sh -c apk add --no-cache not-a-package-at-all\" did not complete successfully: exit code: 1
------
 > [2/2] RUN apk add --no-cache not-a-package-at-all:
1.469 ERROR: unable to select packages:
1.469   not-a-package-at-all (no such package):
1.469     required by: world[not-a-package-at-all]
------
Dockerfile:2
--------------------
   1 |     FROM alpine:3@sha256:3f4d6b32e26703c6082839234f43769d5046b3ac7c910033911d1e71d8cd630a
   2 | >>> RUN apk add --no-cache not-a-package-at-all
   3 |     
--------------------
ERROR: failed to build: failed to solve: process \"/bin/sh -c apk add --no-cache not-a-package-at-all\" did not complete successfully: exit code: 1";

/// The same for a `COPY` whose source is not in the context.
const COPY_FAILURE: &str = "#6 [2/2] COPY missing-file /x
#6 ERROR: failed to calculate checksum of ref ehjfon54b506ntqgmkhofaoke::557hwsb5b5gxwia0u2z24s8xh: \"/missing-file\": not found
------
 > [2/2] COPY missing-file /x:
------
Dockerfile:2
--------------------
   1 |     FROM alpine:3@sha256:3f4d6b32e26703c6082839234f43769d5046b3ac7c910033911d1e71d8cd630a
   2 | >>> COPY missing-file /x
   3 |     
--------------------
ERROR: failed to build: failed to solve: failed to compute cache key: failed to calculate checksum of ref ehjfon54b506ntqgmkhofaoke::557hwsb5b5gxwia0u2z24s8xh: \"/missing-file\": not found";

// ---- the two files, and the names built from them ------------------------------------------

#[test]
fn each_embedded_dockerfile_is_the_file_in_images_and_carries_its_marker() {
    // The const is `include_str!` of the file, so this is the assertion that the file a human edits is the file
    // the binary ships: a move or a rename breaks the build, and a copy that drifted breaks this test.
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("images");
    let pin = include_str!("../../../pins/pi.txt").trim();
    for (file, word, embedded) in [
        ("proxy.Dockerfile", "proxy", images::PROXY_DOCKERFILE),
        ("agent.Dockerfile", "agent", images::AGENT_DOCKERFILE),
    ] {
        let on_disk = std::fs::read_to_string(directory.join(file))
            .unwrap_or_else(|error| panic!("{file} is not readable: {error}"));
        assert_eq!(
            embedded, on_disk,
            "{file} is not the file this binary embeds, so a deploy would build something nobody reviewed"
        );
        // The marker is what `scripts/check-static.sh` greps the built artifact for: it is how the gate proves the
        // recipe is *inside* the binary rather than beside it.
        let marker = format!("# holosmith: embedded {word} image (HS-048)");
        assert!(
            embedded.contains(&marker),
            "the {word} Dockerfile has no marker line `{marker}`"
        );
        // A pinned base by manifest-list digest: a tag would resolve differently on the next pull, and a digest is
        // what makes the same Dockerfile build the same image on two machines.
        assert!(
            embedded
                .lines()
                .any(|line| line.starts_with("FROM ") && line.contains("@sha256:")),
            "the {word} Dockerfile names a base it cannot re-resolve identically"
        );
        assert!(
            embedded.contains(images::BINARY_IN_IMAGE),
            "the {word} Dockerfile copies the binary somewhere else than `BINARY_IN_IMAGE` says"
        );
        // The pinned pi version arrives as a build argument. A literal in the file would be a second source for the
        // pin, which `tests/pin_single_source.rs` refuses.
        assert!(
            !embedded.contains(pin),
            "the {word} Dockerfile spells the pinned pi version ({pin}) instead of taking it as an argument"
        );
    }
    // The two files are different recipes, so a reader must not be able to swap one for the other unnoticed.
    assert_ne!(images::PROXY_DOCKERFILE, images::AGENT_DOCKERFILE);
}

#[test]
fn the_image_names_carry_this_binarys_version() {
    // One spelling of each name: this function is what `agent_image`'s default and both proxies' `image` field
    // read, and a test that restated the format string would pass over a name nobody installs.
    assert_eq!(
        images::proxy_image(),
        format!("holosmith-proxy:{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        images::agent_image(),
        format!("agent-image:{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(images::BINARY_IN_IMAGE, "/usr/local/bin/holosmith");
    assert_eq!(
        [
            images::LABEL_VERSION,
            images::LABEL_GIT_TAG,
            images::LABEL_PI,
            images::LABEL_TOOLCHAINS,
        ],
        [
            "org.holosmith.version",
            "org.holosmith.git-tag",
            "org.holosmith.pi-version",
            "org.holosmith.toolchains",
        ],
        "a label key is a contract with every image already built"
    );
    assert_eq!(Kind::Proxy.tags().len(), 3);
    assert_eq!(Kind::Agent.tags().len(), 4);
    assert_eq!(Kind::Proxy.image(), images::proxy_image());
    assert_eq!(Kind::Agent.image(), images::agent_image());
    assert_eq!(Kind::Proxy.dockerfile(), images::PROXY_DOCKERFILE);
    assert_eq!(Kind::Agent.dockerfile(), images::AGENT_DOCKERFILE);
}

/// The labels the Dockerfile's `LABEL` lines write for `expectation`, as `docker image inspect` answers them.
///
/// Through [`images::build_arguments`] rather than as a second list, so that the argument spelling and the label
/// spelling cannot drift apart in the tests either.
fn labels_of(expectation: &Expectation) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    let args = images::build_arguments("test:image", expectation);
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        if argument != "--build-arg" {
            continue;
        }
        let Some((key, value)) = arguments.next().and_then(|pair| pair.split_once('=')) else {
            continue;
        };
        let label = match key {
            "HOLOSMITH_VERSION" => images::LABEL_VERSION,
            "HOLOSMITH_GIT_TAG" => images::LABEL_GIT_TAG,
            "HOLOSMITH_PI_VERSION" => images::LABEL_PI,
            "HOLOSMITH_TOOLCHAINS" => images::LABEL_TOOLCHAINS,
            _ => continue,
        };
        // The label carries what the argument carried: a `LABEL` line cannot rewrite the string it was given, so
        // the toolchain list is space-separated in the image exactly as it is in `${HOLOSMITH_TOOLCHAINS}`.
        labels.insert(label.to_string(), value.to_string());
    }
    labels
}

#[test]
fn a_mismatch_names_the_key_and_both_values() {
    let config = Config::default();
    let agent = Expectation::of(Kind::Agent, &config);
    // The build arguments are the facts the Dockerfile labels with, and the toolchain list is one word there
    // because `apt-get install` reads it as several packages.
    assert_eq!(
        agent.build_args(),
        [
            (
                "HOLOSMITH_VERSION".to_string(),
                env!("CARGO_PKG_VERSION").to_string()
            ),
            (
                "HOLOSMITH_GIT_TAG".to_string(),
                env!("HOLOSMITH_GIT_TAG").to_string()
            ),
            (
                "HOLOSMITH_PI_VERSION".to_string(),
                holosmith::pins::pi_version().to_string()
            ),
            (
                "HOLOSMITH_TOOLCHAINS".to_string(),
                config.agent_toolchains.join(" ")
            ),
        ]
    );
    let arguments = images::build_arguments("holosmith-proxy:9.9.9", &agent);
    assert_eq!(arguments.first().map(String::as_str), Some("build"));
    assert_eq!(
        &arguments[1..3],
        ["-t".to_string(), "holosmith-proxy:9.9.9".to_string()]
    );
    assert_eq!(
        arguments.last().map(String::as_str),
        Some("-"),
        "the context is the archive on stdin, so `-` is the last argument"
    );

    // Arm one: the labels are this binary's, so there is nothing to say.
    let labels = labels_of(&agent);
    assert_eq!(agent.mismatches(&labels), Vec::<String>::new());
    assert_eq!(
        labels.get(images::LABEL_TOOLCHAINS).map(String::as_str),
        Some("python3 ripgrep")
    );

    // Arm two: a key that is absent, named by its last word and carrying the value that is missing.
    let mut absent = labels.clone();
    absent.remove(images::LABEL_VERSION);
    let sentences = agent.mismatches(&absent);
    assert_eq!(sentences.len(), 1, "{sentences:?}");
    assert!(
        sentences[0].contains("version") && sentences[0].contains("absent"),
        "{}",
        sentences[0]
    );
    assert!(
        sentences[0].contains(env!("CARGO_PKG_VERSION")),
        "the sentence does not say what this binary's value is: {}",
        sentences[0]
    );

    // Arm three: a key that differs, with both values, and only that key.
    let mut different = labels.clone();
    different.insert(
        images::LABEL_PI.to_string(),
        "0.0.0-not-the-pinned-one".to_string(),
    );
    let sentences = agent.mismatches(&different);
    assert_eq!(sentences.len(), 1, "{sentences:?}");
    for fact in [
        "pi-version",
        "0.0.0-not-the-pinned-one",
        holosmith::pins::pi_version(),
    ] {
        assert!(
            sentences[0].contains(fact),
            "the sentence does not carry {fact}: {}",
            sentences[0]
        );
    }

    // Arm four: another toolchain list. The proxy image has no such label at all, so the same image passes its
    // expectation — which is the difference between the two images in one assertion.
    let mut toolchains = labels.clone();
    toolchains.insert(images::LABEL_TOOLCHAINS.to_string(), "python3".to_string());
    let sentences = agent.mismatches(&toolchains);
    assert_eq!(sentences.len(), 1, "{sentences:?}");
    assert!(
        sentences[0].contains("toolchains")
            && sentences[0].contains("python3")
            && sentences[0].contains("python3 ripgrep"),
        "{}",
        sentences[0]
    );
    let proxy = Expectation::of(Kind::Proxy, &config);
    assert_eq!(proxy.mismatches(&toolchains), Vec::<String>::new());
}

// ---- the archive the daemon reads ----------------------------------------------------------

/// A tar header's numeric field: octal digits ended by a NUL or a space, and an all-NUL field is zero — which is
/// how tar spells "this entry is uid 0".
fn octal(field: &[u8]) -> u64 {
    let text = String::from_utf8_lossy(field);
    let digits = text.trim_matches(|c: char| c == '\0' || c == ' ');
    if digits.is_empty() {
        return 0;
    }
    u64::from_str_radix(digits, 8)
        .unwrap_or_else(|error| panic!("{field:?} is not an octal field: {error}"))
}

#[test]
fn the_archive_is_a_ustar_the_tar_program_reads() {
    let dir = TempDir::new("images-archive");
    let binary = dir.file("holosmith");
    // 700 bytes: not a block multiple, so the entry's own padding is exercised rather than assumed.
    let program = vec![b'h'; 700];
    std::fs::write(&binary, &program).expect("the scratch binary is writable");

    let bytes = images::archive(images::PROXY_DOCKERFILE, &binary).expect("the archive is written");

    // 512-byte blocks, and the archive ends with two zero blocks: without them the daemon reads a truncated
    // context and fails with a sentence about a Dockerfile rather than about a missing end.
    assert_eq!(bytes.len() % 512, 0, "{} bytes", bytes.len());
    assert!(
        bytes.ends_with(&[0_u8; 1024]),
        "the archive does not end with the two zero blocks"
    );

    let header = &bytes[..512];
    assert_eq!(
        std::str::from_utf8(&header[..10]).expect("a name is UTF-8"),
        "Dockerfile"
    );
    assert_eq!(octal(&header[100..108]), 0o644);
    // uid, gid and mtime zero: the same Dockerfile and the same binary bytes have to be the same archive, or
    // every rebuild is a different context for the daemon's cache.
    assert_eq!(octal(&header[108..116]), 0);
    assert_eq!(octal(&header[116..124]), 0);
    assert_eq!(octal(&header[136..148]), 0);
    assert_eq!(
        octal(&header[124..136]),
        images::PROXY_DOCKERFILE.len() as u64
    );
    assert_eq!(header[156], b'0', "the entry is a regular file");
    assert_eq!(&header[257..263], b"ustar\0");
    assert_eq!(&header[263..265], b"00");

    let dockerfile_block = images::PROXY_DOCKERFILE.len().div_ceil(512) * 512;
    let second = 512 + dockerfile_block;
    assert_eq!(
        std::str::from_utf8(&bytes[second..second + 9]).expect("a name is UTF-8"),
        "holosmith"
    );
    assert_eq!(octal(&bytes[second + 100..second + 108]), 0o755);
    assert_eq!(octal(&bytes[second + 124..second + 136]), 700);

    // The entry data is the binary's own bytes, and the padding after it is zero.
    let data = second + 512;
    assert_eq!(&bytes[data..data + 700], &program[..]);
    assert!(bytes[data + 700..data + 1024].iter().all(|byte| *byte == 0));
    assert_eq!(
        data + 1024 + 1024,
        bytes.len(),
        "the archive is the two entries and the two zero blocks, and nothing else"
    );

    // The `tar` program is the second reader: a header this crate writes and nothing else can parse would be an
    // archive the daemon also cannot read. `tar` verifies the checksum, which is the one field computed by hand.
    let archive = dir.file("context.tar");
    std::fs::write(&archive, &bytes).expect("the archive is writable");
    let listed = Command::new("tar")
        .arg("-tvf")
        .arg(&archive)
        .output()
        .expect("tar is on every machine this repository builds on");
    assert!(
        listed.status.success(),
        "tar refused the archive: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listing = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listing.contains("Dockerfile") && listing.contains("holosmith"),
        "tar does not list both entries: {listing}"
    );
    assert!(
        listing.contains(" 700 "),
        "tar does not see the binary's 700 bytes: {listing}"
    );
}

#[test]
fn an_archive_of_a_missing_binary_is_a_sentence_before_the_daemon_is_asked() {
    // A build context with no holosmith in it would fail inside the image as a `COPY`, which names the wrong
    // thing; the read fails first, in the words of what is actually wrong.
    let missing = Path::new("/nonexistent/holosmith-not-here");
    let sentence = images::archive(images::PROXY_DOCKERFILE, missing)
        .expect_err("a path that is not a file has no bytes to archive");
    assert!(
        sentence.contains(&missing.display().to_string()),
        "the sentence does not name the path: {sentence}"
    );
    assert!(
        sentence.contains("could not be read"),
        "the sentence does not say what happened: {sentence}"
    );
}

// ---- the failure sentence ------------------------------------------------------------------

#[test]
fn the_failure_sentence_names_the_instruction_the_layer_and_what_the_child_said() {
    let run = images::failure("holosmith-proxy:0.1.0", RUN_FAILURE);
    for fact in [
        "holosmith-proxy:0.1.0",
        "RUN apk add --no-cache not-a-package-at-all",
        "[2/2] RUN",
        "nothing was tagged",
        "did not complete successfully",
    ] {
        assert!(
            run.contains(fact),
            "the sentence does not name {fact}: {run}"
        );
    }
    assert!(
        run.lines().count() <= 3,
        "the sentence is more than a sentence: {run}"
    );
    assert!(
        !run.contains(">>>"),
        "the sentence carries the Dockerfile's instruction marker: {run}"
    );
    assert!(
        !run.contains("FROM alpine"),
        "the sentence carries the Dockerfile around the instruction: {run}"
    );

    // The `COPY` shape: the instruction and the layer, and the child's own last line, which is the one that says
    // what was missing.
    let copy = images::failure("agent-image:0.1.0", COPY_FAILURE);
    for fact in [
        "agent-image:0.1.0",
        "COPY missing-file /x",
        "[2/2] COPY",
        "nothing was tagged",
        "/missing-file",
    ] {
        assert!(
            copy.contains(fact),
            "the sentence does not name {fact}: {copy}"
        );
    }
    assert!(
        copy.chars().count() < 500,
        "the sentence is the build log: {copy}"
    );

    // Output with nothing recognisable in it still says something useful rather than starting with a colon.
    let nothing = images::failure("agent-image:0.1.0", "");
    assert!(nothing.contains("agent-image:0.1.0"), "{nothing}");
    assert!(nothing.contains("printed nothing"), "{nothing}");
    assert!(nothing.contains("nothing was tagged"), "{nothing}");
}

// ---- the read, over a fake -----------------------------------------------------------------

/// A seam that answers one label read and remembers which images it was asked about.
struct Labelled {
    answer: Result<Option<BTreeMap<String, String>>, String>,
    asked: Mutex<Vec<String>>,
}

impl Labelled {
    fn answering(answer: Result<Option<BTreeMap<String, String>>, String>) -> Self {
        Self {
            answer,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl images::Images for Labelled {
    fn daemon_arch(&self) -> Result<String, String> {
        Ok("amd64".to_string())
    }

    fn labels(&self, image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(image.to_string());
        self.answer.clone()
    }

    fn build(&self, tag: &str, _archive: &[u8], _args: &[String]) -> Result<Log, String> {
        Ok(Log {
            success: true,
            text: format!("built {tag}"),
        })
    }
}

#[test]
fn the_read_answers_missing_no_labels_stale_current_and_unreadable() {
    let config = Config::default();
    let expectation = Expectation::of(Kind::Agent, &config);
    let image = images::agent_image();
    let read = |answer| {
        let fake = Labelled::answering(answer);
        let status =
            images::read(&fake, Kind::Agent, &expectation).expect("a read is always an answer");
        (status, fake.asked())
    };

    // Arm one: the image is not there.
    let (status, asked) = read(Ok(None));
    assert_eq!(
        asked,
        [image.as_str()],
        "the read asked about another image"
    );
    match status {
        Status::Missing(reason) => assert!(
            reason.contains(&image) && reason.contains("not on this machine"),
            "{reason}"
        ),
        other => panic!("an absent image read as {other:?}"),
    }

    // Arm two: there, and built by another tool — Docker answers a nil map, which is no labels.
    let (status, _) = read(Ok(Some(BTreeMap::new())));
    match status {
        Status::Stale(reason) => assert!(
            reason.contains("no Holosmith labels")
                && reason.contains("not built from this binary's Dockerfile"),
            "{reason}"
        ),
        other => panic!("an unlabelled image read as {other:?}"),
    }

    // Arm three: the labels are this binary's, which is the only `Current` there is.
    let (status, _) = read(Ok(Some(labels_of(&expectation))));
    assert_eq!(status, Status::Current);

    // Arm four: there and labelled with another value, with the reason naming both.
    let mut stale = labels_of(&expectation);
    stale.insert(
        images::LABEL_GIT_TAG.to_string(),
        "another-build".to_string(),
    );
    let (status, _) = read(Ok(Some(stale)));
    match status {
        Status::Stale(reason) => {
            for fact in ["git-tag", "another-build", env!("HOLOSMITH_GIT_TAG")] {
                assert!(
                    reason.contains(fact),
                    "the reason does not name {fact}: {reason}"
                );
            }
        }
        other => panic!("a mismatched image read as {other:?}"),
    }

    // Arm five: a daemon that cannot answer is a reason for a build, not an error — a dry run has to keep
    // printing its plan on a machine it cannot fully measure.
    let (status, _) = read(Err("permission denied".to_string()));
    match status {
        Status::Stale(reason) => assert!(
            reason.contains("its labels could not be read") && reason.contains("permission denied"),
            "{reason}"
        ),
        other => panic!("an unreadable label read as {other:?}"),
    }
}

#[test]
fn the_architecture_guard_compares_two_vocabularies() {
    // Docker's word and the target triple's are not the same spelling, and a guard that compared them verbatim
    // would fail on every machine.
    let fake = Labelled::answering(Ok(None));
    assert_eq!(
        images::architecture(&fake, "x86_64-unknown-linux-musl"),
        Ok("amd64".to_string())
    );
    let sentence = images::architecture(&fake, "aarch64-unknown-linux-musl")
        .expect_err("an amd64 daemon cannot build for an arm binary");
    for fact in [
        "x86_64",
        "amd64",
        "aarch64",
        "build this machine's artifact",
    ] {
        assert!(
            sentence.contains(fact),
            "the refusal does not name {fact}: {sentence}"
        );
    }
    for fact in [images::proxy_image(), images::agent_image()] {
        assert!(
            sentence.contains(&fact),
            "the refusal does not name {fact}: {sentence}"
        );
    }
}

// ---- the daemon-gated half -----------------------------------------------------------------

/// The static artifact the image arms put in the image: `scripts/build.sh`'s output, or `HOLOSMITH_IMAGE_BIN`.
///
/// A static binary rather than this test's own `current_exe`, and the reason is the image's purpose: the proxy
/// image is an alpine base with no glibc, so a debug host binary copied into it would build and then fail to start
/// — which is the failure the artfiact's target triple exists to prevent.
fn artifact() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HOLOSMITH_IMAGE_BIN") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]
        .into_iter()
        .flat_map(|target| {
            [
                workspace.join("dist").join(target).join("holosmith"),
                workspace
                    .join("target")
                    .join(target)
                    .join("release/holosmith"),
            ]
        })
        .find(|path| path.is_file())
}

/// The words an artifact's own `version` line reports: `version`, `git_tag` and `pi_version`.
fn reported(binary: &Path) -> BTreeMap<String, String> {
    let output = Command::new(binary)
        .arg("version")
        .output()
        .unwrap_or_else(|error| panic!("{} could not be run: {error}", binary.display()));
    assert!(
        output.status.success(),
        "{} version answered {:?}: {}",
        binary.display(),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(key, value)| (key.to_string(), value.trim().to_string()))
        .collect()
}

/// Whether the daemon answers and there is an artifact to build from, printing one line when it does not.
fn ready() -> bool {
    let daemon = Command::new("docker")
        .args(["version", "--format", "{{.Server.Arch}}"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !daemon {
        eprintln!(
            "SKIP tests/images.rs: no Docker daemon answers (the image builds are case_server_images)"
        );
        return false;
    }
    if artifact().is_none() {
        eprintln!(
            "SKIP tests/images.rs: no static holosmith artifact — run scripts/build.sh (the image builds are \
             case_server_images)"
        );
        return false;
    }
    true
}

/// The tags this test owns. Test-only names, so a run here cannot change what a deploy's step 3 reads.
const TAG: &str = "holosmith-proxy:test-images";
const BROKEN: &str = "holosmith-proxy:test-images-broken";

/// `docker` with `arguments`, answering whether it succeeded and what it said.
fn docker(arguments: &[&str]) -> (bool, String) {
    let output = Command::new("docker")
        .args(arguments)
        .output()
        .expect("docker is on the PATH of a machine whose daemon answers");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), said)
}

#[test]
fn a_built_proxy_image_runs_both_proxies_and_a_broken_build_leaves_no_tag() {
    if !ready() {
        return;
    }
    let binary = artifact().expect("ready() checked the artifact");
    let daemon = Docker::new(None);
    // The artifact's own values rather than this test binary's: the image is built from *that* binary, and the
    // claim under test is that the Dockerfile writes the arguments it is given into the labels.
    let reported = reported(&binary);
    let expectation = Expectation {
        version: reported
            .get("version")
            .expect("the artifact reports its version")
            .clone(),
        git_tag: reported
            .get("git_tag")
            .expect("the artifact reports its git tag")
            .clone(),
        pi_version: reported
            .get("pi_version")
            .expect("the artifact reports its pi version")
            .clone(),
        toolchains: None,
    };
    assert_eq!(
        expectation.pi_version,
        holosmith::pins::pi_version(),
        "the artifact was built for another pin than this crate"
    );

    // The arm owns the absent state, so it starts by clearing its own tags — never the production name.
    let _ = docker(&["image", "rm", "-f", TAG]);
    let _ = docker(&["image", "rm", "-f", BROKEN]);

    let context =
        images::archive(images::PROXY_DOCKERFILE, &binary).expect("the context is written");
    let arguments = images::build_arguments(TAG, &expectation);
    let built = <Docker as images::Images>::build(&daemon, TAG, &context, &arguments)
        .expect("the daemon was asked");
    assert!(
        built.success,
        "the proxy image did not build: {}",
        images::failure(TAG, &built.text)
    );

    // The labels, read back the way step 3 reads them: the whole point of the build arguments.
    let labels = <Docker as images::Images>::labels(&daemon, TAG)
        .expect("the daemon answers an inspect")
        .expect("the image that just built is there");
    assert_eq!(
        expectation.mismatches(&labels),
        Vec::<String>::new(),
        "the image's labels are not the arguments it was built with: {labels:?}"
    );
    assert!(
        !labels.contains_key(images::LABEL_TOOLCHAINS),
        "the proxy image carries the agent image's toolchain label: {labels:?}"
    );

    // And it runs: both proxies are this one binary behind one entrypoint.
    for subcommand in [["proxy", "egress", "--help"], ["proxy", "key", "--help"]] {
        let (success, said) = docker(&[
            "run",
            "--rm",
            "--network",
            "none",
            "--entrypoint",
            images::BINARY_IN_IMAGE,
            TAG,
            subcommand[0],
            subcommand[1],
            subcommand[2],
        ]);
        assert!(
            success,
            "`{} {}` did not run from the image: {said}",
            subcommand[0], subcommand[1]
        );
    }

    // A Dockerfile that cannot build, through the production path: the sentence names the instruction, and nothing
    // is tagged. The `FROM` line is the proxy Dockerfile's own, so the pinned base is not a second literal.
    let base = images::PROXY_DOCKERFILE
        .lines()
        .find(|line| line.starts_with("FROM "))
        .expect("the proxy Dockerfile starts from a pinned base");
    let broken = format!("{base}\nCOPY missing-file /x\n");
    let context = images::archive(&broken, &binary).expect("the context is written");
    let arguments = images::build_arguments(BROKEN, &expectation);
    let failed = <Docker as images::Images>::build(&daemon, BROKEN, &context, &arguments)
        .expect("the daemon was asked");
    assert!(
        !failed.success,
        "a Dockerfile that copies a file the context does not hold built anyway"
    );
    let sentence = images::failure(BROKEN, &failed.text);
    assert!(
        sentence.contains("COPY missing-file /x"),
        "the failure does not name the instruction: {sentence}"
    );
    assert!(
        sentence.contains("nothing was tagged"),
        "the failure does not say nothing was tagged: {sentence}"
    );
    let (there, _) = docker(&["image", "inspect", BROKEN]);
    assert!(
        !there,
        "a failed build left {BROKEN} behind, so the next deploy's read would compare it"
    );

    // Both tags go, and the layer cache stays: the next run of this test is cheap because the base and the
    // package layer are already there.
    let _ = docker(&["image", "rm", "-f", TAG]);
    let _ = docker(&["image", "rm", "-f", BROKEN]);
}
