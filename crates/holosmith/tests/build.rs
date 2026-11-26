//! The builder container's rules that need a filesystem or an ELF header: the placement, its artifact check,
//! and the one invariant that ties its two pinned bases to the agent image's.
//!
//! What is *pure* — the argv vectors, the recipe's own digest, the tail, the archive — is a unit test in
//! `src/builder.rs`, where the constants live. What is here is what a daemon would otherwise be the only witness of:
//! a directory that already holds a different binary, an artifact for the wrong machine, and a source tree that is
//! not one. The gate's `case_build` is the third witness, and it is the only one that runs a container.

mod support;

use std::path::Path;

use holosmith::builder::{self, Placement};
use holosmith::images;
use support::TempDir;

/// A file's SHA-256, as `holosmith build` computes it: `images::digest`, the one producer of a release digest.
fn digest_of(path: &Path) -> String {
    images::digest(path).expect("the file is readable")
}

/// A minimal 64-bit little-endian ELF header, optionally with one `PT_INTERP` program header.
fn elf(machine: u16, kind: u16, interpreter: bool) -> Vec<u8> {
    let mut bytes = vec![0_u8; 64 + if interpreter { 56 } else { 0 }];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2; // EI_CLASS: 64-bit
    bytes[5] = 1; // EI_DATA: little-endian
    bytes[16..18].copy_from_slice(&kind.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes()); // e_phoff: right after the header
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes()); // e_phentsize
    bytes[56..58].copy_from_slice(&(u16::from(interpreter)).to_le_bytes());
    if interpreter {
        bytes[64..68].copy_from_slice(&3_u32.to_le_bytes()); // PT_INTERP
    }
    bytes
}

#[test]
fn the_builder_and_the_agent_image_share_one_node_base() {
    // Two recipes name the same base digest, so a node that moves in one of them must move in both. A test rather
    // than a comment: the second copy is the one nobody edits.
    let agent = digest_after(images::AGENT_DOCKERFILE, "FROM node:");
    let builder = digest_after(builder::DOCKERFILE, "FROM node:");
    assert!(!agent.is_empty() && !builder.is_empty());
    assert_eq!(
        agent, builder,
        "the builder's node base and the agent image's are the same digest"
    );
}

/// The `@sha256:…` the `FROM <prefix>…` line carries, whether or not the line names a stage.
fn digest_after(dockerfile: &str, prefix: &str) -> String {
    dockerfile
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(prefix))
        .find_map(|line| {
            line.split_whitespace()
                .nth(1)?
                .rsplit_once("@sha256:")
                .map(|(_, digest)| digest.to_string())
        })
        .unwrap_or_default()
}

#[test]
fn an_artifact_for_the_requested_target_is_accepted() {
    let path = Path::new("/out/holosmith");
    for (machine, target) in [(62_u16, builder::X86_64), (183, builder::AARCH64)] {
        let bytes = elf(machine, 2, false);
        assert_eq!(
            builder::artifact_problem(path, &bytes, target),
            None,
            "{target}"
        );
    }
}

#[test]
fn an_artifact_for_another_machine_is_refused() {
    let said = builder::artifact_problem(
        Path::new("/out/holosmith"),
        &elf(183, 2, false),
        builder::X86_64,
    )
    .expect("an aarch64 build is not an x86_64 artifact");
    assert!(said.contains("aarch64"), "{said}");
    assert!(said.contains(builder::X86_64), "{said}");
}

#[test]
fn an_artifact_with_a_dynamic_loader_or_a_pie_type_is_refused() {
    let path = Path::new("/out/holosmith");
    let interp = builder::artifact_problem(path, &elf(62, 2, true), builder::X86_64)
        .expect("a PT_INTERP segment means it needs a loader");
    assert!(interp.contains("PT_INTERP"), "{interp}");
    // ET_DYN is what an exported RUSTFLAGS ships (ADR-0005).
    let pie = builder::artifact_problem(path, &elf(62, 3, false), builder::X86_64)
        .expect("ET_DYN is a static-PIE artifact");
    assert!(pie.contains("ADR-0005"), "{pie}");
}

#[test]
fn something_that_is_not_an_elf_is_refused() {
    let said = builder::artifact_problem(
        Path::new("/out/holosmith"),
        b"#!/bin/sh\necho not a binary\n",
        builder::X86_64,
    )
    .expect("a shell script is not a release artifact");
    assert!(said.contains("not an ELF"), "{said}");
}

#[test]
fn a_fresh_directory_gets_the_artifact_and_its_digest() {
    let dir = TempDir::new("build-fresh");
    let out = dir.path();
    std::fs::write(out.join(builder::PENDING), b"binary").expect("the container's file");
    let digest = digest_of(&out.join(builder::PENDING));

    assert_eq!(builder::place(out, &digest), Ok(Placement::Fresh));
    assert_eq!(
        std::fs::read(out.join(builder::BINARY)).expect("the artifact"),
        b"binary"
    );
    assert_eq!(
        std::fs::read_to_string(out.join(builder::DIGEST)).expect("the digest file"),
        format!("{digest}  {}\n", builder::BINARY),
        "the digest file is `sha256sum -c`'s format"
    );
    assert!(
        !out.join(builder::PENDING).exists(),
        "the pending file never survives a placement"
    );
}

#[test]
fn the_same_bytes_again_are_idempotent() {
    let dir = TempDir::new("build-current");
    let out = dir.path();
    std::fs::write(out.join(builder::PENDING), b"binary").expect("the container's file");
    let digest = digest_of(&out.join(builder::PENDING));
    assert_eq!(builder::place(out, &digest), Ok(Placement::Fresh));

    // A re-run of the same tree: the container writes the same bytes again, and nothing is replaced.
    std::fs::write(out.join(builder::PENDING), b"binary").expect("the second build");
    let written = std::fs::metadata(out.join(builder::BINARY))
        .and_then(|metadata| metadata.modified())
        .expect("the artifact's mtime");
    assert_eq!(builder::place(out, &digest), Ok(Placement::Current));
    assert_eq!(
        std::fs::metadata(out.join(builder::BINARY))
            .and_then(|metadata| metadata.modified())
            .expect("the artifact's mtime"),
        written,
        "an idempotent re-run does not touch the artifact"
    );
    assert!(!out.join(builder::PENDING).exists());
}

#[test]
fn a_directory_with_the_right_bytes_but_no_digest_gets_one() {
    // The digest write can fail after a successful rename — a full disk — and `upgrade` reads two files, so "already
    // this build" has to mean both of them are there rather than only the bytes.
    let dir = TempDir::new("build-current-no-digest");
    let out = dir.path();
    std::fs::write(out.join(builder::BINARY), b"binary").expect("the artifact");
    let digest = digest_of(&out.join(builder::BINARY));
    assert_eq!(builder::place(out, &digest), Ok(Placement::Current));
    assert_eq!(
        std::fs::read_to_string(out.join(builder::DIGEST)).expect("the digest file"),
        format!("{digest}  {}\n", builder::BINARY)
    );
}

#[test]
fn a_different_binary_in_the_same_release_directory_is_refused() {
    let dir = TempDir::new("build-different");
    let out = dir.path();
    std::fs::write(out.join(builder::PENDING), b"first").expect("the first build");
    let first = digest_of(&out.join(builder::PENDING));
    assert_eq!(builder::place(out, &first), Ok(Placement::Fresh));

    std::fs::write(out.join(builder::PENDING), b"second").expect("the second build");
    let second = digest_of(&out.join(builder::PENDING));
    assert_eq!(
        builder::place(out, &second),
        Ok(Placement::Different {
            existing: first.clone(),
            built: second,
        })
    );
    assert_eq!(
        std::fs::read(out.join(builder::BINARY)).expect("the artifact"),
        b"first",
        "a refusal replaces nothing"
    );
    assert_eq!(
        std::fs::read_to_string(out.join(builder::DIGEST)).expect("the digest file"),
        format!("{first}  {}\n", builder::BINARY),
        "and leaves the digest of what is really there"
    );
    assert!(
        !out.join(builder::PENDING).exists(),
        "the refused build's bytes are not left behind"
    );
}

#[test]
fn a_directory_that_is_not_a_source_tree_is_refused_before_anything_runs() {
    let dir = TempDir::new("build-source");
    let said =
        builder::source_problem(dir.path()).expect("an empty directory is not a Holosmith tree");
    assert!(said.contains("not a Holosmith source tree"), "{said}");

    std::fs::create_dir_all(dir.path().join("scripts")).expect("scripts/");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    assert!(
        builder::source_problem(dir.path()).is_some(),
        "no build.sh yet"
    );
    std::fs::write(dir.path().join("scripts/build.sh"), "#!/bin/sh\n").expect("build.sh");
    assert_eq!(builder::source_problem(dir.path()), None);
}

#[test]
fn a_builder_image_that_fell_back_is_named_in_the_line_the_command_prints() {
    // The builder recipe is built by `builder::ensure_image` and by nothing else, and it has no sink: the sentence it
    // returns is the whole of what an operator running `holosmith build` hears about a fallback (ADR-0116 D3). Driven
    // through the command, in both shapes, because the line is the command's — a log that warned and one that did not,
    // the second of which must be the line this command has always printed.
    let image = builder::image();
    let warning = format!(
        "{} (curl exit 60): retrying that URL without certificate verification, and the pinned digest decides",
        images::CERTIFICATE_WARNING
    );
    let warned = builder_line(
        "build-warn",
        &format!("#8 0.512 {warning}\n#9 0.512 {warning}\n#8 DONE 9.1s"),
    );
    // The clause repeats the marker's own words without its `warning: ` prefix, so a clause appended twice would name
    // the certificate twice: one is the whole of what a build may say about it, however many of the recipe's two
    // fetches fell back.
    let named = images::CERTIFICATE_WARNING.trim_start_matches("warning: ");
    assert_eq!(
        warned.matches(named).count(),
        1,
        "the line does not name the certificate exactly once: {warned}"
    );
    assert!(
        warned.starts_with(&format!("{image} was built from this recipe; ")),
        "the fallback is not the sentence's own clause: {warned}"
    );
    assert!(
        warned.ends_with("fetched without verification and its digest matched"),
        "the clause does not say what the pin did: {warned}"
    );
    assert_eq!(
        builder_line("build-quiet", "#8 DONE 9.1s"),
        format!("{image} was built from this recipe"),
        "a build that warned nothing changed the line this command prints"
    );
}

/// The `build: builder: ` line one `holosmith build` run prints, over a stand-in daemon whose builder-image build
/// answers `log`.
///
/// The stand-in's `run` step fails, and that is deliberate: a stand-in cannot produce the static artifact this side
/// checks, and the builder step's line is printed before the container ever runs. The failure is asserted so the line
/// found here cannot be one from a command that stopped earlier than the one meant.
fn builder_line(label: &str, log: &str) -> String {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new(label);
    let source = dir.file("source");
    std::fs::create_dir_all(source.join("scripts")).expect("scripts/");
    std::fs::write(source.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    std::fs::write(source.join("scripts/build.sh"), "#!/bin/sh\n").expect("build.sh");
    let lines = dir.write("build.log", log);
    let bin = dir.file("bin");
    std::fs::create_dir_all(&bin).expect("bin/");
    let docker = bin.join("docker");
    // The three questions a build asks, in `case_build`'s order: who the daemon is (this machine's own word, as the
    // gate's stand-in answers it), whether the image is there (it is not), and what a build prints. The log is a file
    // rather than a heredoc so the warning's own apostrophe needs no quoting, and the archive on stdin is drained the
    // way a real daemon reads it.
    std::fs::write(
        &docker,
        "#!/bin/sh\n\
         if [ \"${1:-}\" = \"-H\" ]; then shift 2; fi\n\
         case \"${1:-} ${2:-}\" in\n\
           \"version --format\")\n\
             case \"$(uname -m)\" in\n\
               aarch64 | arm64) printf 'arm64\\n' ;;\n\
               *) printf 'amd64\\n' ;;\n\
             esac ;;\n\
           \"image inspect\") exit 1 ;;\n\
           \"build -t\") cat >/dev/null; cat \"$HS_BUILD_LINES\" ;;\n\
           *) exit 1 ;;\n\
         esac\n",
    )
    .expect("the stand-in");
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755))
        .expect("the stand-in is executable");
    // 0644 like every fixture config the gate writes: a mode is what `doctor` reads, and one spelling of a fixture
    // config is cheaper than remembering which readers care.
    let config = dir.write("config.toml", "role = \"hub\"\n");
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).expect("0644");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .args(["build", "--config"])
        .arg(&config)
        .arg("--out")
        .arg(dir.file("out"))
        .current_dir(&source)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HS_BUILD_LINES", &lines)
        .output()
        .expect("the command runs");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !output.status.success() && stderr.contains("the build of"),
        "the stand-in's run step did not fail where this reads its line after: {stderr}"
    );
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("build: builder: "))
        .unwrap_or_else(|| panic!("the command printed no builder line: {stdout}{stderr}"))
        .to_string()
}

#[test]
fn the_default_target_is_one_of_the_two_release_triples() {
    let target = builder::default_target().expect("this test runs on a machine this build targets");
    assert!(
        target == builder::X86_64 || target == builder::AARCH64,
        "{target}"
    );
    assert_eq!(builder::target_of("amd64"), Some(builder::X86_64));
    assert_eq!(builder::target_of("arm64"), Some(builder::AARCH64));
    assert_eq!(builder::target_of("riscv64"), None);
}
