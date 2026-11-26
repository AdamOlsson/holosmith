//! The three embedded Dockerfiles, the label read and the build's own shapes (ADR-0057).
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

use holosmith::docker::Docker;
use holosmith::images::{self, Expectation, Kind, Log, Status};
use holosmith::version::VersionInfo;

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

/// The shape a build takes when musl.cc does not answer: the layer, curl's own diagnostic for a connection that never
/// opened and its retry warning, the instruction with the whole `RUN` around it, and the exit status.
///
/// **Recorded before the recipes grew their certificate fallback, and left exactly as it was recorded.** The
/// instruction it echoes is `RUN curl -fsSL …` where a build of the current recipes prints `RUN if curl …`. That is
/// drift in the recording, not in the rule: `images::fetch_did_not_answer` reads the host's name and the build's last
/// exit status, and never the instruction's text, so what this measures is the shape a *pinned fetch* takes.
const MUSL_FETCH_FAILURE: &str = "#8 [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz
#8 0.512 curl: (7) Failed to connect to musl.cc port 443 after 0 ms: Couldn't connect to server
#8 0.512 Warning: Transient problem: connection refused Will retry in 2 seconds. 4 retries left.
------
 > [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz:
0.512 curl: (28) Connection timed out after 300000 milliseconds
------
Dockerfile:76
--------------------
  76 | >>> RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz \\
  77 |       https://musl.cc/x86_64-linux-musl-cross.tgz \\
--------------------
ERROR: failed to build: failed to solve: process \"/bin/sh -c curl -fsSL -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz\" did not complete successfully: exit code: 28";

/// The tarball arrived and was not the pinned one: the failure the pin exists for, and the one a fetch classification
/// may never excuse. It carries a retried attempt's connection error as well, which is what makes the digest the thing
/// this arm is about. Its instruction is the pre-fallback `RUN curl -fsSL …` shape, for the reason
/// [`MUSL_FETCH_FAILURE`] gives; the digest is what this arm reads, not an instruction.
const DIGEST_MISMATCH: &str = "#8 0.512 curl: (7) Failed to connect to musl.cc port 443 after 0 ms: Couldn't connect to server
------
 > [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz:
0.512 /tmp/musl.tgz: FAILED
0.512 sha256sum: WARNING: 1 computed checksum did NOT match
------
Dockerfile:76
--------------------
  76 | >>> RUN curl -fsSL -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz \\
--------------------
ERROR: failed to build: failed to solve: process \"/bin/sh -c curl -fsSL -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz\" did not complete successfully: exit code: 1";

/// The name that did not resolve: the log ADR-0111 D1 quotes from a real `docker build` under `--network none`, five
/// attempts two seconds apart and the child's own status 6. Its instruction is the pre-fallback `RUN curl -fsSL …`
/// shape, for the reason [`MUSL_FETCH_FAILURE`] gives: the status is what this arm reads.
const MUSL_NAME_UNRESOLVED: &str = "#8 [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz
#8 0.166 curl: (6) Could not resolve host: musl.cc
#8 2.170 curl: (6) Could not resolve host: musl.cc
#8 ERROR: process \"/bin/sh -c curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz\" did not complete successfully: exit code: 6
------
 > [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz:
0.166 curl: (6) Could not resolve host: musl.cc
------
Dockerfile:76
--------------------
  76 | >>> RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz \\
--------------------
ERROR: failed to build: failed to solve: process \"/bin/sh -c curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz\" did not complete successfully: exit code: 6";

/// The host answering an HTTP 5xx: curl's `-f` status 22, and the one 22 that is the host declining rather than the
/// recipe naming a tarball that is not there.
const MUSL_HTTP_DECLINED: &str = "#8 [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz
#8 0.412 curl: (22) The requested URL returned error: 503
#8 2.412 curl: (22) The requested URL returned error: 503
#8 ERROR: process \"/bin/sh -c curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz\" did not complete successfully: exit code: 22
------
 > [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz:
0.412 curl: (22) The requested URL returned error: 503
------
Dockerfile:76
--------------------
  76 | >>> RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz \\
--------------------
ERROR: failed to build: failed to solve: process \"/bin/sh -c curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz\" did not complete successfully: exit code: 22";

/// The same 22 with a 404 behind it: the recipe asking for a tarball that host does not serve, which is this tree's
/// defect and must stay one.
const MUSL_TARBALL_NOT_FOUND: &str = "#8 [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz
#8 0.412 curl: (22) The requested URL returned error: 404
#8 ERROR: process \"/bin/sh -c curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz\" did not complete successfully: exit code: 22
------
 > [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz:
0.412 curl: (22) The requested URL returned error: 404
------
Dockerfile:76
--------------------
  76 | >>> RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz \\
--------------------
ERROR: failed to build: failed to solve: process \"/bin/sh -c curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz\" did not complete successfully: exit code: 22";

/// Half of a build log this classification has to ignore: the fetch retried, printed curl's two diagnostics per attempt,
/// and then **got its bytes** (`#8 DONE`). `--retry-all-errors` makes that an ordinary shape on a day the host is away,
/// which is why the rule reads the build's final status rather than searching the log's text.
const MUSL_RETRY_THEN_SUCCEEDED: &str = "#8 [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz
#8 0.512 curl: (7) Failed to connect to musl.cc port 443 after 0 ms: Couldn't connect to server
#8 0.512 Warning: Transient problem: connection refused Will retry in 2 seconds. 4 retries left.
#8 2.700 curl: (7) Failed to connect to musl.cc port 443 after 0 ms: Couldn't connect to server
#8 2.700 Warning: Transient problem: connection refused Will retry in 2 seconds. 3 retries left.
#8 DONE 9.4s";

/// That same fetch, and then the layer that really failed: a `RUN` whose rustc error is the tree's own defect, ending
/// with the status that says so. Before the rule was anchored on that status this log read as an outage.
const MUSL_RETRIED_THEN_COMPILE_FAILURE: &str = "#8 [4/9] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz
#8 0.512 curl: (7) Failed to connect to musl.cc port 443 after 0 ms: Couldn't connect to server
#8 0.512 Warning: Transient problem: connection refused Will retry in 2 seconds. 4 retries left.
#8 DONE 9.4s
#9 [5/9] RUN cargo build --release --locked
#9 1.204 error[E0425]: cannot find value `missing` in this scope
#9 ERROR: process \"/bin/sh -c cargo build --release --locked\" did not complete successfully: exit code: 1
------
 > [5/9] RUN cargo build --release --locked:
1.204 error[E0425]: cannot find value `missing` in this scope
------
Dockerfile:82
--------------------
  82 | >>> RUN cargo build --release --locked
--------------------
ERROR: failed to build: failed to solve: process \"/bin/sh -c cargo build --release --locked\" did not complete successfully: exit code: 1";

/// `curl` failing inside an image that is not fetching this build's pinned tarballs: that image's own failure, which
/// this classification exists not to excuse.
const OTHER_CURL_FAILURE: &str = "------
 > [2/2] RUN curl -fsSL https://registry.example/thing.json:
0.512 curl: (6) Could not resolve host: registry.example
------
Dockerfile:2
--------------------
   2 | >>> RUN curl -fsSL https://registry.example/thing.json
--------------------
ERROR: failed to build: failed to solve: process \"/bin/sh -c curl -fsSL https://registry.example/thing.json\" did not complete successfully: exit code: 6";

// ---- the three files, and the names built from them ------------------------------------------

#[test]
fn each_embedded_dockerfile_is_the_file_in_images_and_carries_its_marker() {
    // The const is `include_str!` of the file, so this is the assertion that the file a human edits is the file
    // the binary ships: a move or a rename breaks the build, and a copy that drifted breaks this test.
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("images");
    let pin = include_str!("../../../pins/pi.txt").trim();
    // The recipe's place for the binary, and the one recipe that has none. `true` is the two images the proxy
    // containers' `--entrypoint` reaches; the base image copies nothing, and that is asserted in its own direction
    // below rather than excused from the loop: an assertion that skipped it would not notice a `COPY` appearing.
    for (file, word, embedded, copies_binary) in [
        ("proxy.Dockerfile", "proxy", images::PROXY_DOCKERFILE, true),
        ("base.Dockerfile", "base", images::BASE_DOCKERFILE, false),
        ("agent.Dockerfile", "agent", images::AGENT_DOCKERFILE, true),
    ] {
        let on_disk = std::fs::read_to_string(directory.join(file))
            .unwrap_or_else(|error| panic!("{file} is not readable: {error}"));
        assert_eq!(
            embedded, on_disk,
            "{file} is not the file this binary embeds, so a deploy would build something nobody reviewed"
        );
        // The marker is what `scripts/check-static.sh` greps the built artifact for: it is how the gate proves the
        // recipe is *inside* the binary rather than beside it.
        let marker = format!("# holosmith: embedded {word} image");
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
        if copies_binary {
            assert!(
                embedded.contains(images::BINARY_IN_IMAGE),
                "the {word} Dockerfile copies the binary somewhere else than `BINARY_IN_IMAGE` says"
            );
        } else {
            assert!(
                !embedded.contains("COPY") && !embedded.contains(images::BINARY_IN_IMAGE),
                "the {word} Dockerfile copies something into an image whose contract is that it holds no program of \
                 ours: the binary's three `--entrypoint` sites are all proxy containers, and the in-container \
                 `container snapshot` entry point has no caller in the product path"
            );
        }
        // The pinned pi version arrives as a build argument. A literal in the file would be a second source for the
        // pin, which `tests/pin_single_source.rs` refuses.
        assert!(
            !embedded.contains(pin),
            "the {word} Dockerfile spells the pinned pi version ({pin}) instead of taking it as an argument"
        );
    }
    // The files are different recipes, so a reader must not be able to swap one for another unnoticed.
    assert_ne!(images::PROXY_DOCKERFILE, images::AGENT_DOCKERFILE);
    assert_ne!(images::BASE_DOCKERFILE, images::AGENT_DOCKERFILE);
    assert_ne!(images::BASE_DOCKERFILE, images::PROXY_DOCKERFILE);
}

#[test]
fn the_base_recipe_is_the_agent_images_base_with_this_repositorys_toolchain_absent() {
    // **Same base, by the same digest.** A session that moves from the base image to the agent image must not move to
    // another distribution, and the smoke image's `the_smoke_recipe_names_the_agent_images_base` reads the agent
    // recipe's last `FROM`: one line feeds both, so the comparison is against that line and not against the rust
    // stage above it.
    let last_base = |recipe: &str| -> String {
        recipe
            .lines()
            .rfind(|line| line.starts_with("FROM "))
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(
        last_base(images::BASE_DOCKERFILE),
        last_base(images::AGENT_DOCKERFILE),
        "the base image's base is not the agent image's"
    );

    // **The absence is the contract**, so it is asserted rather than assumed: nothing this recipe *does* installs a
    // compiler, a Rust toolchain, a cross linker, a crate set or an npm cache. The instructions are read rather than
    // the whole file, because the header names each of them in the sentence that says why it is not here.
    let blocks = instructions(images::BASE_DOCKERFILE);
    let recipe = blocks.join(" ");
    for absent in [
        "build-essential",
        "rustup",
        "cargo",
        "file",
        "cargo-vendor",
        "npm-cache",
        ".cargo/config.toml",
    ] {
        assert!(
            !recipe.contains(absent),
            "the base recipe carries `{absent}` in an instruction, which is the agent image's half of the contract"
        );
    }
    // What it does carry, each entry a program some code already spawns or probes for.
    let (_, apt) = instruction(&blocks, "apt-get install");
    for package in ["ca-certificates", "git", "openssl", "procps"] {
        assert!(
            apt.split_whitespace().any(|word| word == package),
            "the base recipe's apt line does not install {package}: {apt}"
        );
    }
    // pi at the pinned version, as the build argument `agent::version_of` compares against `pins/pi.txt`.
    assert!(
        images::BASE_DOCKERFILE.contains("@earendil-works/pi-coding-agent@${HOLOSMITH_PI_VERSION}"),
        "the base recipe does not install pi at the version it is handed"
    );
    // `PI_CODING_AGENT_DIR` is an absolute path because a session's `HOME` is `/` and belongs to no writable place
    // (ADR-0087): nothing in this image may need `$HOME`.
    assert!(
        images::BASE_DOCKERFILE.contains("PI_CODING_AGENT_DIR=/tmp/pi-agent"),
        "the base recipe leaves pi's agent directory at the `$HOME` a session does not have"
    );
}

// ---- the agent recipe's shape, asserted without a daemon --------------------------------------

/// The Dockerfile's instructions, each as one line with its continuation and argument lines joined by single spaces.
///
/// Just enough of the syntax for the assertions below: a line that begins at column zero with an uppercase
/// instruction word starts one, and the lines continued with `\` and the indented ones belong to it. A comment line
/// belongs to none of them, however it is indented — Dockerfile comments are not part of the instruction they sit in,
/// and a comment folded into a `RUN` would make its prose read as something that step spends — as does a blank line.
/// This is what lets a test ask *which* instruction writes a path, and whether it does so before another.
fn instructions(recipe: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    for line in recipe.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let word = line.split_once(' ').map_or("", |(word, _)| word);
        if !line.starts_with([' ', '\t'])
            && !word.is_empty()
            && word.chars().all(|letter| letter.is_ascii_uppercase())
        {
            blocks.push(line.trim_end_matches('\\').trim().to_string());
        } else if line.starts_with([' ', '\t'])
            && let Some(current) = blocks.last_mut()
        {
            current.push(' ');
            current.push_str(line.trim_end_matches('\\').trim());
        }
    }
    blocks
}

/// The first instruction containing `needle`, with its position: order is what half of the assertions are about.
fn instruction<'a>(blocks: &'a [String], needle: &str) -> (usize, &'a str) {
    let at = blocks
        .iter()
        .position(|block| block.contains(needle))
        .unwrap_or_else(|| panic!("the agent recipe has no instruction containing {needle:?}"));
    (at, blocks[at].as_str())
}

/// The word after `prefix` in `text`: how `CARGO_HOME=/tmp/cargo` and `--cache /opt/npm-cache` are read.
fn word_after<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.split(prefix)
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("nothing follows {prefix} in {text}"))
}

/// Every word after `prefix` in `text`: one instruction can name a variable twice, and both are words to look at.
fn words_after<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.split(prefix)
        .skip(1)
        .filter_map(|rest| rest.split_whitespace().next())
        .collect()
}

/// The TOML value written as `key = "value"`.
fn quoted<'a>(text: &'a str, key: &str) -> &'a str {
    let opener = format!("{key} = \"");
    let start = text
        .find(&opener)
        .unwrap_or_else(|| panic!("no `{opener}…` in {text}"))
        + opener.len();
    let rest = &text[start..];
    &rest[..rest
        .find('"')
        .unwrap_or_else(|| panic!("the value after `{opener}` is never closed in {text}"))]
}

/// The `HOLOSMITH_…` names an instruction mentions, per name and with the prefix put back: how a recipe's own text
/// says which build arguments an instruction reads.
fn argument_names(text: &str) -> Vec<String> {
    text.split("HOLOSMITH_")
        .skip(1)
        .map(|rest| {
            let name = rest
                .split(|letter: char| {
                    !(letter.is_ascii_uppercase() || letter.is_ascii_digit() || letter == '_')
                })
                .next()
                .unwrap_or_default();
            format!("HOLOSMITH_{name}")
        })
        .collect()
}

#[test]
fn the_agent_recipe_vendors_its_crates_and_points_cargo_at_them() {
    // A Dockerfile-shaped claim, so it is asserted here and not in a case that needs a daemon: the order of two
    // instructions and the agreement between a path written in one and the path another one names are exactly what a
    // mis-edit breaks, and what the gate's `case_agent_toolchain` would report as a network failure five minutes in.
    let blocks = instructions(images::AGENT_DOCKERFILE);

    // The context and the recipe name the same files, in **both** directions: a context path no `COPY` names is a file
    // the image silently never gets, and a `COPY` of a path the context does not carry is the daemon's "not found" —
    // the direction that fails a build.
    let copies: Vec<&str> = blocks
        .iter()
        .filter(|block| block.starts_with("COPY "))
        .map(String::as_str)
        .collect();
    for (path, _) in Kind::Agent.context() {
        assert!(
            copies
                .iter()
                .any(|copy| copy.split_whitespace().any(|word| word == *path)),
            "the agent recipe never copies {path} out of the context it is handed"
        );
    }
    let context: Vec<&str> = Kind::Agent
        .context()
        .iter()
        .map(|(path, _)| *path)
        .collect();
    for copy in &copies {
        // `COPY --from=<stage>` reads another stage rather than the context.
        if copy.contains("--from=") {
            continue;
        }
        let words: Vec<&str> = copy.split_whitespace().skip(1).collect();
        let (_, sources) = words
            .split_last()
            .unwrap_or_else(|| panic!("a `COPY` names no destination: {copy}"));
        // `holosmith` is the one word that is not a `Kind::Agent.context()` entry: the binary the build places there.
        for source in sources {
            assert!(
                *source == "holosmith" || context.contains(source),
                "the agent recipe copies {source}, which the context it is handed does not carry: {copy}"
            );
        }
    }

    // `file` is what `scripts/check-static.sh` asks about an artifact first (`file -b`), and `scripts/build.sh` ends
    // with that script: an image without it builds a tree and then fails the build's own last step.
    let (_, apt) = instruction(&blocks, "apt-get install");
    assert!(
        apt.split_whitespace().any(|word| word == "file"),
        "the apt line installs no `file`: {apt}"
    );

    // The vendor step, and the directory it writes, which the config below has to agree with.
    let (vendored_at, vendor) = instruction(&blocks, "cargo vendor --locked");
    let vendored = word_after(vendor, "--locked");
    assert!(
        vendored.starts_with('/'),
        "`cargo vendor --locked` writes to {vendored}, which is not an absolute path"
    );
    // The scratch `CARGO_HOME` is this step's own and is removed inside the same instruction: the layer must not
    // carry the 108 MB of `.crate` archives beside the 108 MB of unpacked crates. Twice, because a path mentioned
    // once is a path still in the image.
    let scratch = word_after(vendor, "CARGO_HOME=");
    assert_ne!(
        scratch, "/tmp/cargo",
        "the vendor step writes its archives into the session's CARGO_HOME"
    );
    assert_eq!(
        vendor.matches(scratch).count(),
        2,
        "the vendor step sets {scratch} and does not remove it in the same instruction: {vendor}"
    );
    // The digest of the lock the crates came from, written beside them: what says a vendor directory is this tree's.
    assert!(
        vendor.contains(&format!("{vendored}/.holosmith-cargo-lock")),
        "the vendor step records no lock digest in {vendored}: {vendor}"
    );

    // **The order is load-bearing**: the replacement is written after the directory it names exists, because a
    // `directory` source pointing nowhere fails with a message about the configuration rather than about the vendor
    // step that has not run.
    let (config_at, config) = instruction(&blocks, "> /.cargo/config.toml");
    assert!(
        config_at > vendored_at,
        "the crate source replacement is written before the directory it points at"
    );
    assert_eq!(
        quoted(config, "directory"),
        vendored,
        "the written config's `directory` is not the directory the vendor step writes"
    );
    assert_eq!(quoted(config, "replace-with"), "vendored-sources");
    assert!(
        config.contains("[net]") && config.contains("offline = true"),
        "the written config does not make `net.offline` true: {config}"
    );

    // A session's own `CARGO_HOME` is still the writable `/tmp/cargo`, and exactly one instruction names it: the vendor
    // step's scratch home shares the prefix, so this counts words and not instructions, and a second word that is
    // exactly `/tmp/cargo` is a second place a session's cargo is pointed at.
    let homes = blocks
        .iter()
        .filter(|block| words_after(block, "CARGO_HOME=").contains(&"/tmp/cargo"))
        .count();
    assert_eq!(
        homes, 1,
        "the session's CARGO_HOME is named by {homes} instructions"
    );

    // The npm half: the cache the fill writes is the one the `ENV` names, so `npm ci` in a session reads it with no
    // flag — and the fill comes after the `npm cache clean --force` that would otherwise leave it empty.
    let (filled_at, fill) = instruction(&blocks, "npm ci --cache");
    let cache = word_after(fill, "--cache");
    let (cleaned_at, _) = instruction(&blocks, "npm cache clean --force");
    assert!(
        filled_at > cleaned_at,
        "the npm cache is filled before the `npm cache clean --force` that empties it"
    );
    let (_, npm_env) = instruction(&blocks, "npm_config_cache=");
    assert_eq!(
        word_after(npm_env, "npm_config_cache="),
        cache,
        "the `npm_config_cache` a session reads is not the cache the fill writes"
    );
    assert!(
        npm_env.contains("npm_config_offline=true"),
        "the image does not make npm's `offline` true: {npm_env}"
    );
    assert!(
        npm_env.contains("npm_config_logs_dir=/tmp/npm-logs"),
        "npm's logs directory is the read-only cache: {npm_env}"
    );
}

/// The instructions that fetch one of the two musl tarballs, in file order: one per toolchain, in each recipe.
///
/// The filter is the pinned URL and not `RUN curl `: a fetch begins `RUN if curl …` (the certificate fallback,
/// ADR-0116), and a filter on the older spelling would measure no downloads at all and pass over an empty list.
fn toolchain_fetches(name: &str, recipe: &str) -> Vec<String> {
    let fetches: Vec<String> = instructions(recipe)
        .into_iter()
        .filter(|block| block.contains("https://musl.cc/"))
        .collect();
    assert_eq!(
        fetches.len(),
        2,
        "{name} does not fetch exactly two tarballs: {fetches:?}"
    );
    fetches
}

#[test]
fn both_recipes_retry_a_toolchain_host_that_does_not_answer() {
    // The two musl cross toolchains are fetched from musl.cc while each image builds, so an image build is only as
    // available as that host (ADR-0111). A bounded retry is the whole of the recipe's half, and `--retry` alone is not
    // it: `--retry` retries a timeout and an HTTP 5xx and not the refused connection or the name that did not resolve,
    // which is what an outage looks like from inside a build. An edit that dropped one of the four words would put the
    // gate back to recording a network blip as a tree nobody passed, so both files are read and both downloads are
    // asserted — with the digest check that stays the pin beside them.
    for (name, recipe) in [
        ("agent.Dockerfile", images::AGENT_DOCKERFILE),
        ("builder.Dockerfile", holosmith::builder::DOCKERFILE),
    ] {
        for download in toolchain_fetches(name, recipe) {
            for word in [
                "--retry 4",
                "--retry-delay 2",
                "--retry-all-errors",
                "https://musl.cc/",
                "| sha256sum -c -",
            ] {
                assert!(
                    download.contains(word),
                    "{name}'s download does not carry `{word}`: {download}"
                );
            }
        }
    }
}

#[test]
fn a_label_only_build_argument_is_declared_below_every_run() {
    // A `RUN` is in scope of every `ARG` declared above it, and BuildKit puts that scope in the instruction's cache
    // key whether or not the instruction names the value — so an argument no `RUN` reads is a cache boundary every
    // layer above it pays for. A worker's tree always changes the git tag and the artifact's digest, and the three
    // label-only arguments sat at the top of all three recipes once: every gate run then rebuilt the toolchain, both
    // pinned musl.cc fetches, the pi install, `cargo vendor` and `npm ci` for values nothing above `LABEL` reads
    // (ADR-0115). The base image is in the loop with the other two: `server deploy`'s step 3 builds it on every
    // machine, and its three label-only arguments are the agent image's three.
    //
    // The label-only set is *derived* from the recipe rather than listed here, so an argument a later edit adds to a
    // `LABEL` is checked without this test being edited. `HOLOSMITH_PI_VERSION` is not in it: the pi install spends
    // it, so its value does change what the image holds.
    for (name, kind, recipe) in [
        ("agent.Dockerfile", Kind::Agent, images::AGENT_DOCKERFILE),
        ("proxy.Dockerfile", Kind::Proxy, images::PROXY_DOCKERFILE),
        ("base.Dockerfile", Kind::Base, images::BASE_DOCKERFILE),
    ] {
        let blocks = instructions(recipe);
        let last_run = blocks
            .iter()
            .rposition(|block| block.starts_with("RUN "))
            .unwrap_or_else(|| panic!("{name} runs nothing, so nothing can be cached"));
        let labels = blocks
            .iter()
            .find(|block| block.starts_with("LABEL "))
            .unwrap_or_else(|| {
                panic!("{name} writes no label, so a deploy has nothing to compare")
            });

        // The labels a deploy's skip rule and `doctor` read are still all written — four on every image — which is
        // the half of the recipe a moved `ARG` could silently drop.
        for tag in kind.tags() {
            assert!(
                labels.contains(&format!("{tag}=")),
                "{name}'s `LABEL` block writes no {tag}, so a deploy reads an image whose facts it cannot judge: \
                 {labels}"
            );
        }

        let spent_by_a_run: Vec<String> = blocks
            .iter()
            .filter(|block| block.starts_with("RUN "))
            .flat_map(|block| argument_names(block))
            .collect();
        let label_only: Vec<String> = argument_names(labels)
            .into_iter()
            .filter(|argument| !spent_by_a_run.contains(argument))
            .collect();
        assert!(
            !label_only.is_empty(),
            "{name} has no argument only its `LABEL` block reads, so this test measured nothing: {labels}"
        );
        for argument in label_only {
            let declared_at = blocks.iter().position(|block| {
                block
                    .strip_prefix("ARG ")
                    .is_some_and(|declared| declared == argument.as_str())
            });
            let declared_at = declared_at.unwrap_or_else(|| {
                panic!("{name} writes {argument} into a label and declares no such argument")
            });
            assert!(
                declared_at > last_run,
                "{argument} is declared above a `RUN` in {name}, and a `RUN` is in scope of every argument declared \
                 above it: a tree whose only change is that value rebuilds every layer below it — in `agent.Dockerfile` \
                 measured as 42–64 s of apt, both pinned musl.cc fetches, the pi install and the vendored crate set — for \
                 a value only a `LABEL` reads"
            );
        }
    }
}

#[test]
fn both_recipes_read_an_unverifiable_certificate_as_a_warning_over_the_pin() {
    // musl.cc's certificate expired and stayed unrenewed, which failed every from-scratch build of both images with
    // curl's 60 — over tarballs the same `RUN` pins by SHA-256 (ADR-0116). The fallback is only ever reached when the
    // certificate is what failed, it only ever adds `--insecure` to the same URL, and the digest check still follows it:
    // the pin is what says the bytes are the vetted ones. Each of those three is a fact this asserts, and a comment
    // beside a recipe is not.
    for (name, recipe) in [
        ("agent.Dockerfile", images::AGENT_DOCKERFILE),
        ("builder.Dockerfile", holosmith::builder::DOCKERFILE),
    ] {
        for fetch in toolchain_fetches(name, recipe) {
            // One fallback and not a fetch made without verification: the first attempt is curl's own flags, and
            // `--insecure` is the retry's.
            assert_eq!(
                fetch.matches("--insecure").count(),
                1,
                "{name}'s fetch is not exactly one verified attempt and one fallback: {fetch}"
            );
            let urls: Vec<usize> = fetch
                .match_indices("https://musl.cc/")
                .map(|(at, _)| at)
                .collect();
            assert_eq!(
                urls.len(),
                2,
                "{name}'s fallback does not retry the same one tarball: {fetch}"
            );
            let insecure = fetch
                .find("--insecure")
                .unwrap_or_else(|| panic!("{name}'s fetch has no fallback: {fetch}"));
            assert!(
                urls[0] < insecure && insecure < urls[1],
                "{name}'s `--insecure` is not the second attempt at the same URL: {fetch}"
            );
            // The fallback is guarded by curl's certificate statuses and by nothing else: every other status is exited
            // with the child's own code, which is what keeps a host that did not answer a failed build (ADR-0111).
            assert!(
                fetch.contains("case $status in 51|60|77|83)"),
                "{name}'s fallback is not guarded by curl's four certificate statuses alone: {fetch}"
            );
            assert!(
                fetch.contains("*) exit $status"),
                "{name}'s fallback does not leave every other curl status fatal: {fetch}"
            );
            // The pin still decides what lands in `/opt`, and it follows the fallback rather than replacing it.
            let digest = fetch
                .find("| sha256sum -c -")
                .unwrap_or_else(|| panic!("{name}'s fetch checks no digest: {fetch}"));
            assert!(
                insecure < digest,
                "{name}'s digest check does not follow the fallback it has to vouch for: {fetch}"
            );
            // And the fallback says so, on stderr, in the words each build's reader carries to an operator.
            assert!(
                fetch.contains(images::CERTIFICATE_WARNING) && fetch.contains(">&2"),
                "{name}'s fallback prints no warning an operator would hear: {fetch}"
            );
        }
    }
}

#[test]
fn the_image_names_carry_this_binarys_version() {
    // One spelling of each name: this function is what `base_image`'s default and every proxy's `image` field
    // read, and a test that restated the format string would pass over a name nobody installs.
    assert_eq!(
        images::proxy_image(),
        format!("holosmith-proxy:{}", holosmith::VERSION)
    );
    assert_eq!(
        images::base_image(),
        format!("holosmith-base:{}", holosmith::VERSION)
    );
    assert_eq!(
        images::agent_image(),
        format!("agent-image:{}", holosmith::VERSION)
    );
    assert_eq!(images::BINARY_IN_IMAGE, "/usr/local/bin/holosmith");
    assert_eq!(
        [
            images::LABEL_VERSION,
            images::LABEL_GIT_TAG,
            images::LABEL_PI,
            images::LABEL_BINARY,
        ],
        [
            "org.holosmith.version",
            "org.holosmith.git-tag",
            "org.holosmith.pi-version",
            "org.holosmith.binary-sha256",
        ],
        "a label key is a contract with every image already built"
    );
    // No image carries a toolchain label any more, because the recipes install a fixed package set: there is no
    // list a machine could change, so there is nothing for a label to carry and nothing for a skip rule to compare.
    for kind in [Kind::Proxy, Kind::Base, Kind::Agent] {
        assert_eq!(
            kind.tags(),
            [
                images::LABEL_VERSION,
                images::LABEL_GIT_TAG,
                images::LABEL_PI,
                images::LABEL_BINARY,
            ],
            "the {kind:?} image's label set is not the four every image has"
        );
    }
    assert_eq!(Kind::Proxy.image(), images::proxy_image());
    assert_eq!(Kind::Base.image(), images::base_image());
    assert_eq!(Kind::Agent.image(), images::agent_image());
    assert_eq!(Kind::Proxy.dockerfile(), images::PROXY_DOCKERFILE);
    assert_eq!(Kind::Base.dockerfile(), images::BASE_DOCKERFILE);
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
            "HOLOSMITH_BINARY_SHA256" => images::LABEL_BINARY,
            _ => continue,
        };
        // The label carries what the argument carried: a `LABEL` line cannot rewrite the string it was given.
        labels.insert(label.to_string(), value.to_string());
    }
    labels
}

/// An `Expectation` built from this test binary's own bytes, which is what `Expectation::of` demands.
///
/// The digest is a read of a real file, so no test can construct an image's expected labels without one — that is
/// the point of the digest label (ADR-0057).
fn expectation(kind: Kind) -> Expectation {
    // The product's own memoised digest of this process: the test binary is tens of megabytes and an unoptimised
    // SHA-256 is slow, and the bytes cannot change under a single process.
    let digest = images::own_digest().expect("the test binary is readable");
    Expectation::of_digest(kind, &digest)
}

#[test]
fn a_mismatch_names_the_key_and_both_values() {
    let agent = expectation(Kind::Agent);
    // The build arguments are the facts the Dockerfile labels with, and them alone: every one of them is written by
    // a `LABEL` line and read back by the skip rule.
    assert_eq!(
        agent.build_args(),
        [
            (
                "HOLOSMITH_VERSION".to_string(),
                holosmith::VERSION.to_string()
            ),
            (
                "HOLOSMITH_GIT_TAG".to_string(),
                env!("HOLOSMITH_GIT_TAG").to_string()
            ),
            (
                "HOLOSMITH_PI_VERSION".to_string(),
                holosmith::pins::pi_version().to_string()
            ),
            ("HOLOSMITH_BINARY_SHA256".to_string(), agent.binary.clone()),
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
        labels.get(images::LABEL_PI).map(String::as_str),
        Some(holosmith::pins::pi_version())
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
        sentences[0].contains(holosmith::VERSION),
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

    // Arm four: a label this binary does not write is none of the read's business, on any image. A project builds an
    // image of its own from a `docker/Dockerfile` and may label it as it likes, and an image built by a release that
    // still wrote a toolchain list is not stale *for that label* — the four it does carry decide it.
    let mut extra = labels.clone();
    extra.insert(
        "org.holosmith.toolchains".to_string(),
        "python3".to_string(),
    );
    extra.insert("org.example.project".to_string(), "mine".to_string());
    for kind in [Kind::Proxy, Kind::Base, Kind::Agent] {
        let other = expectation(kind);
        assert_eq!(other.mismatches(&extra), Vec::<String>::new(), "{kind:?}");
    }
}

// ---- the build arguments the daemon is handed ----------------------------------------------

#[test]
fn the_agent_build_carries_the_rust_pin_and_the_other_images_carry_no_such_argument() {
    // The rust base's reference is no longer a literal in a `FROM` line: an `ARG` is the only mechanism a Dockerfile
    // has for a shared value, so the *binary* is what hands the pin over, and only to the image whose recipe has a
    // rust stage (ADR-0118). Two facts follow, and each is a mistake a later edit could make: an argument the agent's
    // vector dropped would fail the build before any layer runs, on the stage's base reference, and one the proxy's
    // or the base's vector gained would be a `--build-arg` no recipe declares — a BuildKit warning over nothing.
    let wanted = format!(
        "{}={}",
        images::RUST_IMAGE_ARG,
        holosmith::pins::rust_image()
    );
    for kind in [Kind::Proxy, Kind::Base, Kind::Agent] {
        let expectation = expectation(kind);
        let arguments = images::build_arguments(&kind.image(), &expectation);
        let carried = arguments
            .iter()
            .filter(|argument| *argument == &wanted)
            .count();
        let rust_arguments = arguments
            .iter()
            .filter(|argument| argument.starts_with(images::RUST_IMAGE_ARG))
            .count();
        if kind == Kind::Agent {
            assert_eq!(
                carried, 1,
                "the agent build does not carry the rust pin once: {arguments:?}"
            );
        } else {
            assert_eq!(
                rust_arguments, 0,
                "the {kind:?} recipe has no rust stage, and its build carries an argument nothing reads: \
                 {arguments:?}"
            );
        }
    }
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

/// `tar`'s answer for one archive this test wrote, listed with `flag`.
///
/// The second reader, and it is not decoration: a header this crate writes and nothing else can parse would be an
/// archive the daemon also cannot read. `tar` verifies the checksum, which is the one field computed by hand.
fn tar_lists(dir: &TempDir, name: &str, bytes: &[u8], flag: &str) -> String {
    let archive = dir.file(name);
    std::fs::write(&archive, bytes).expect("the archive is writable");
    let listed = Command::new("tar")
        .arg(flag)
        .arg(&archive)
        .output()
        .expect("tar is on every machine this repository builds on");
    assert!(
        listed.status.success(),
        "tar refused {}: {}",
        archive.display(),
        String::from_utf8_lossy(&listed.stderr)
    );
    String::from_utf8_lossy(&listed.stdout).into_owned()
}

#[test]
fn the_archive_is_a_ustar_the_tar_program_reads() {
    let dir = TempDir::new("images-archive");
    let binary = dir.file("holosmith");
    // 700 bytes: not a block multiple, so the entry's own padding is exercised rather than assumed.
    let program = vec![b'h'; 700];
    std::fs::write(&binary, &program).expect("the scratch binary is writable");

    // The agent image, because it is the one whose context carries the six files: eight entries — its Dockerfile,
    // its binary, and those six.
    let context = Kind::Agent.context();
    let bytes = images::archive(images::AGENT_DOCKERFILE, context, &binary)
        .expect("the archive is written");
    // Three identical inputs, one archive: uid, gid and mtime stay zero and nothing reads a clock, or every
    // rebuild would be a new context for the daemon's cache.
    assert_eq!(
        bytes,
        images::archive(images::AGENT_DOCKERFILE, context, &binary)
            .expect("the archive is written")
    );

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
    // uid, gid and mtime zero: the same Dockerfile, the same binary bytes and the same context have to be the
    // same archive, or every rebuild is a different context for the daemon's cache.
    assert_eq!(octal(&header[108..116]), 0);
    assert_eq!(octal(&header[116..124]), 0);
    assert_eq!(octal(&header[136..148]), 0);
    assert_eq!(
        octal(&header[124..136]),
        images::AGENT_DOCKERFILE.len() as u64
    );
    assert_eq!(header[156], b'0', "the entry is a regular file");
    assert_eq!(&header[257..263], b"ustar\0");
    assert_eq!(&header[263..265], b"00");

    let dockerfile_block = images::AGENT_DOCKERFILE.len().div_ceil(512) * 512;
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

    // Then the context's entries, in the order [`Kind::context`] gives them: each under its real relative path,
    // a 0644 regular file whose data is that file's text. This is where a build vendors the crate set from, so a
    // lock that never reached the context is a failure here as well as on disk.
    let mut at = data + 1024;
    for &(path, contents) in context {
        let header = &bytes[at..at + 512];
        assert_eq!(
            std::str::from_utf8(&header[..path.len()]).expect("a name is UTF-8"),
            path
        );
        assert_eq!(octal(&header[100..108]), 0o644);
        assert_eq!(octal(&header[108..116]), 0);
        assert_eq!(octal(&header[116..124]), 0);
        assert_eq!(octal(&header[136..148]), 0);
        assert_eq!(octal(&header[124..136]), contents.len() as u64);
        assert_eq!(header[156], b'0', "{path} is not a regular file");
        let data = at + 512;
        assert_eq!(&bytes[data..data + contents.len()], contents.as_bytes());
        let block = contents.len().div_ceil(512) * 512;
        assert!(
            bytes[data + contents.len()..data + block]
                .iter()
                .all(|byte| *byte == 0),
            "the padding after {path} is not zero"
        );
        at = data + block;
    }
    assert_eq!(
        at + 1024,
        bytes.len(),
        "the archive is the Dockerfile, the binary, the context's {} entries and the two zero blocks, and \
         nothing else",
        context.len()
    );

    // `tar` reads the same eight entries, in the same order, and sees the binary's own 700 bytes rather than a
    // block.
    let names: Vec<String> = tar_lists(&dir, "agent-context.tar", &bytes, "-tf")
        .lines()
        .map(str::to_string)
        .collect();
    let wanted: Vec<String> = std::iter::once("Dockerfile")
        .chain(std::iter::once("holosmith"))
        .chain(context.iter().map(|(path, _)| *path))
        .map(str::to_string)
        .collect();
    assert_eq!(
        names, wanted,
        "tar does not list the agent image's eight entries in the archive's order"
    );
    let listing = tar_lists(&dir, "agent-context.tar", &bytes, "-tvf");
    assert!(
        listing.contains(" 700 "),
        "tar does not see the binary's 700 bytes: {listing}"
    );

    // The proxy and base images' contexts are the two entries alone: neither recipe installs a Rust build, so there
    // is no crate set for either to read. The base image's archive still holds the binary's bytes because the
    // archive is one shape for every kind — and it is the one recipe that never `COPY`s them, so what a build sends
    // and what the image holds are two different questions for it.
    for (kind, name) in [(Kind::Proxy, "proxy"), (Kind::Base, "base")] {
        let sent = images::archive(kind.dockerfile(), kind.context(), &binary)
            .expect("the archive is written");
        let names: Vec<String> = tar_lists(&dir, &format!("{name}-context.tar"), &sent, "-tf")
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(
            names,
            ["Dockerfile", "holosmith"],
            "the {name} image's context is not the two entries"
        );
    }
}

#[test]
fn the_agent_context_is_the_six_files_the_tree_has_on_disk() {
    // A lock or manifest edit that does not reach the image would be a build vendoring another tree's crates, so
    // every embedded entry is compared with the file at the path it is written under: the archive's copy and this
    // repository's are one text. The list is asserted first, because a renamed path would otherwise only move what
    // the comparison reads.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let context = Kind::Agent.context();
    let paths: Vec<&str> = context.iter().map(|(path, _)| *path).collect();
    assert_eq!(
        paths,
        [
            "Cargo.toml",
            "Cargo.lock",
            "crates/holosmith/Cargo.toml",
            "crates/holosmith/src/lib.rs",
            "web/package.json",
            "web/package-lock.json",
        ],
        "the agent image's build context is not the six files a build can vendor from"
    );
    for &(path, embedded) in context {
        let on_disk = std::fs::read_to_string(root.join(path)).unwrap_or_else(|error| {
            panic!("{path} is not readable under {}: {error}", root.display())
        });
        assert_eq!(
            embedded,
            on_disk.as_str(),
            "{path} in the binary is not the file on disk, so an edit did not reach the image"
        );
    }
    // The proxy and base images build no Rust, so their contexts have no second entry to send.
    for kind in [Kind::Proxy, Kind::Base] {
        assert!(
            kind.context().is_empty(),
            "the {kind:?} image's context carries entries its recipe cannot read"
        );
    }
}

#[test]
fn an_archive_of_a_missing_binary_is_a_sentence_before_the_daemon_is_asked() {
    // A build context with no holosmith in it would fail inside the image as a `COPY`, which names the wrong
    // thing; the read fails first, in the words of what is actually wrong.
    let missing = Path::new("/nonexistent/holosmith-not-here");
    let sentence = images::archive(images::PROXY_DOCKERFILE, &[], missing)
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
        "no new image was tagged",
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
        "no new image was tagged",
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
    assert!(nothing.contains("no new image was tagged"), "{nothing}");
}

#[test]
fn a_build_whose_pinned_fetch_did_not_answer_is_not_a_verdict_about_the_tree() {
    // The recipes fetch both musl cross toolchains while the image builds, so musl.cc being away is a build that never
    // started. `server deploy` exits 4 for it and the gate prints a line that measured nothing (ADR-0111), and both of
    // those rest on this classification: one log read for two facts — was it a fetch, and what does the sentence say.
    //
    // The endings curl itself can produce for a transfer that never arrived: the wait that timed out, the name that did
    // not resolve (the shape ADR-0111 D1 quotes from a real build), and an HTTP 5xx, which is the host declining rather
    // than the recipe naming a tarball that is not there.
    for (said, log) in [
        ("a connection that timed out", MUSL_FETCH_FAILURE),
        ("a name that did not resolve", MUSL_NAME_UNRESOLVED),
        ("an HTTP 503 from the host", MUSL_HTTP_DECLINED),
    ] {
        assert!(
            images::fetch_did_not_answer(log),
            "{said} is not a fetch that did not answer"
        );
    }
    let sentence = images::failure("agent-image:0.1.0", MUSL_FETCH_FAILURE);
    for fact in [
        "agent-image:0.1.0",
        "musl.cc",
        "did not answer",
        "not a recipe or a tree that is wrong",
        "no new image was tagged",
        "exit code: 28",
    ] {
        assert!(
            sentence.contains(fact),
            "the sentence does not name {fact}: {sentence}"
        );
    }
    assert!(
        sentence.lines().count() <= 3,
        "the sentence is more than a sentence: {sentence}"
    );

    // The failures that are the tree's own, each of which must stay a plain exit 1: a package that is not there, a
    // `COPY` of a file the context does not carry, a tarball that arrived and was not the pinned one (the one this
    // classification exists **not** to excuse, because a digest mismatch is what the pin is for), a 404 behind curl's
    // 22, and — the log this rule is anchored for — a fetch that retried, printed curl's diagnostics and **got its
    // bytes**, followed by a layer whose rustc error is the defect.
    for (said, log) in [
        ("a missing package", RUN_FAILURE),
        ("a missing context file", COPY_FAILURE),
        ("a tarball that failed its digest", DIGEST_MISMATCH),
        ("a tarball the host does not have", MUSL_TARBALL_NOT_FOUND),
        (
            "a retried fetch and then a compile error",
            MUSL_RETRIED_THEN_COMPILE_FAILURE,
        ),
    ] {
        assert!(
            !images::fetch_did_not_answer(log),
            "{said} reads as a fetch that did not answer"
        );
        assert!(
            !images::failure("agent-image:0.1.0", log).contains("did not answer"),
            "{said} is reported as a host that did not answer: {}",
            images::failure("agent-image:0.1.0", log)
        );
    }

    // The same retried fetch in front of a `COPY` of a file the archive does not carry, which is the commonest way a
    // build fails after a fetch that retried. BuildKit's status for that one names no exit code at all, so the log has
    // no final status to find: a build the recipe failed is not a fetch.
    let after_copy = format!("{MUSL_RETRY_THEN_SUCCEEDED}\n{COPY_FAILURE}");
    assert!(
        !images::fetch_did_not_answer(&after_copy),
        "a retried fetch excused a COPY of a file the context does not carry: {after_copy}"
    );

    // And `curl` failing inside an image that fetches something of its own is that image's failure: the host's name has
    // to be in the log as well as the status, or every network failure anywhere would be excused here.
    assert!(
        !images::fetch_did_not_answer(OTHER_CURL_FAILURE),
        "a fetch of another host reads as this build's pinned toolchain download"
    );
    // A build whose fetch succeeded is not a fetch that did not answer, whatever it printed on the way.
    assert!(
        !images::fetch_did_not_answer(MUSL_RETRY_THEN_SUCCEEDED),
        "a retried fetch that got its bytes reads as one that did not answer"
    );
    assert!(!images::fetch_did_not_answer(""));
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
    let expectation = expectation(Kind::Agent);
    let image = images::agent_image();
    let read = |answer| {
        let fake = Labelled::answering(answer);
        let status = images::read(&fake, &expectation);
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
                // "this binary's" would be the wrong subject now that one read answers for another release's
                // images as well: the sentence names the **version** whose build it compared against.
                && reason.contains(&format!(
                    "not built from version {}'s Dockerfile",
                    expectation.version
                )),
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
    for fact in [
        images::proxy_image(),
        images::base_image(),
        images::agent_image(),
    ] {
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
    // The host's own triple first: a Dockerfile whose base resolves to the *daemon's* architecture builds an
    // image whose entrypoint is the artifact copied into it, so an x86_64 artifact on an arm64 machine is a
    // container that cannot `exec` — the mistake `images::architecture` refuses on the deploy path (ADR-0057).
    let mut targets = vec![env!("HOLOSMITH_TARGET")];
    targets.extend(
        ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]
            .into_iter()
            .filter(|target| *target != env!("HOLOSMITH_TARGET")),
    );
    targets
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
        kind: Kind::Proxy,
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
        binary: images::digest(&binary).expect("the artifact is readable"),
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
        images::archive(images::PROXY_DOCKERFILE, &[], &binary).expect("the context is written");
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
        !labels.contains_key("org.holosmith.toolchains"),
        "the proxy image carries a toolchain label, which no recipe writes: {labels:?}"
    );

    // And it runs: every proxy is this one binary behind one entrypoint.
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
    let context = images::archive(&broken, &[], &binary).expect("the context is written");
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
        sentence.contains("no new image was tagged"),
        "the failure does not say no new image was tagged: {sentence}"
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

// ---- what a promotion must build (spec item 10's acceptance bullet 4) ---------------------------------

/// A seam that answers each image's labels from a map, so one read can see two releases at once.
struct Tagged {
    labels: BTreeMap<String, BTreeMap<String, String>>,
    asked: Mutex<Vec<String>>,
}

impl Tagged {
    fn answering(labels: BTreeMap<String, BTreeMap<String, String>>) -> Self {
        Self {
            labels,
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

impl images::Images for Tagged {
    fn daemon_arch(&self) -> Result<String, String> {
        Ok("amd64".to_string())
    }

    fn labels(&self, image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(image.to_string());
        Ok(self.labels.get(image).cloned())
    }

    fn build(&self, tag: &str, _archive: &[u8], _args: &[String]) -> Result<Log, String> {
        Ok(Log {
            success: true,
            text: format!("built {tag}"),
        })
    }
}

/// The release a promotion is about: another version than the running binary's, and its own pin, so a read that
/// used `env!` would answer the wrong tags and the wrong labels.
fn promoted() -> VersionInfo {
    VersionInfo {
        version: "9.9.9".to_string(),
        git_tag: "v9.9.9".to_string(),
        // A candidate being promoted need not be running out of a release directory: `release` is the tag of the
        // directory a file is *in*, not of the build.
        release: None,
        pi_version: holosmith::pins::pi_version().to_string(),
        schema_version: holosmith::schema::SCHEMA_VERSION,
        target: env!("HOLOSMITH_TARGET").to_string(),
        sqlite: String::new(),
    }
}

/// The images' label maps for `release`, keyed by tag, the way `docker image inspect` would answer them.
fn labelled(release: &VersionInfo, digest: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut images_by_tag = BTreeMap::new();
    for kind in [Kind::Proxy, Kind::Base, Kind::Agent] {
        let expectation = Expectation::of_release(kind, release, digest);
        images_by_tag.insert(expectation.tag(), labels_of(&expectation));
    }
    images_by_tag
}

#[test]
fn a_promotion_builds_what_the_release_needs_and_nothing_else() {
    let digest = images::own_digest().expect("the test binary is readable");
    let release = promoted();

    // Arm one: every image already carries this release's labels. Nothing to build, and the read asked about the
    // **candidate's** tags — `holosmith-proxy:9.9.9`, not the running binary's 0.1.0 — in the order they are built.
    let fake = Tagged::answering(labelled(&release, &digest));
    let pending = images::outstanding(&fake, &release, &digest);
    assert!(pending.is_empty(), "{pending:?}");
    assert_eq!(
        fake.asked(),
        [
            format!("holosmith-proxy:{}", release.version),
            format!("holosmith-base:{}", release.version),
            format!("agent-image:{}", release.version)
        ],
        "the read asked about the wrong tags"
    );

    // Arm two: a different binary's bytes. Every image is stale — the digest is the one input a tag cannot
    // describe (ADR-0057), and a promotion has to rebuild for the bytes it is making live. The base image's label
    // counts here even though its recipe copies no binary: the label is the build's fact, and a rule that skipped it
    // there would leave a promotion serving an image built from the previous artifact.
    let fake = Tagged::answering(labelled(&release, "0".repeat(64).as_str()));
    let pending = images::outstanding(&fake, &release, &digest);
    assert_eq!(pending.len(), 3, "{pending:?}");
    for (kind, reason) in &pending {
        assert!(
            reason.contains("binary-sha256"),
            "the reason for {kind:?} does not name the digest: {reason}"
        );
    }

    // Arm three: the pin changed. **Every** recipe carries it (the proxy image labels the pin its Dockerfile was
    // built against, because a proxy that reaches a provider is the thing a pin bump is usually about), so all three
    // are stale — the acceptance's "or a pinned version changed", measured in its own direction.
    let mut newer_pin = release.clone();
    newer_pin.pi_version = "0.0.0-not-the-pin".to_string();
    let fake = Tagged::answering(labelled(&release, &digest));
    let pending = images::outstanding(&fake, &newer_pin, &digest);
    assert_eq!(pending.len(), 3, "{pending:?}");
    for (kind, reason) in &pending {
        assert!(
            reason.contains("pi-version"),
            "the reason for {kind:?} does not name the pin: {reason}"
        );
    }

    // Arm four: a version this machine has never built. Every tag is missing, which is a build rather than a
    // comparison — the same answer a first deploy gives.
    let mut newer = release.clone();
    newer.version = "10.0.0".to_string();
    let fake = Tagged::answering(BTreeMap::new());
    let pending = images::outstanding(&fake, &newer, &digest);
    assert_eq!(pending.len(), 3, "{pending:?}");
    for (_, reason) in &pending {
        assert!(reason.contains("is not on this machine"), "{reason}");
    }
}
