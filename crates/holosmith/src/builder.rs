//! The pinned builder container: the one place a release artifact is compiled (HS-076).
//!
//! Spec item 10's first step is that the build never runs on the host, so that a `build.rs`, a proc macro — or the
//! web app's own Vite config, which is JavaScript the build executes — cannot run outside a container. This module
//! is the container half of that sentence and nothing else: the recipe (`images/builder.Dockerfile` and its entry
//! script, both `include_str!`ed like the two product recipes), the two `docker` calls that run it, and the rules
//! that decide what the output directory may hold. The command that prints is [`crate::commands::build`].
//!
//! Four things are load-bearing and each has its own doc comment below:
//!
//! - **The image is ours and it is pinned twice.** Both bases are pinned by manifest-list digest, and the two
//!   musl.cc toolchain tarballs are pinned by their SHA-256 — a download is a pin only when its bytes are checked.
//!   The recipe is labelled with its own digest, so editing it without bumping the version rebuilds instead of
//!   reusing a stale image (the same argument as `org.holosmith.binary-sha256`, ADR-0057).
//! - **The source is mounted read-only and the build happens in a copy.** A build cannot reach the operator's tree:
//!   not `target/`, not `node_modules/`, not the repository itself. The only host path it writes is the output
//!   directory, which is a bind mount of its own.
//! - **The container's output goes to a file, never a pipe.** A build prints thousands of lines; a pipe nobody
//!   drains fills at 64 KiB and deadlocks (HS-048's landmine). [`crate::docker::Docker::run_captured`] is the
//!   primitive, and the failure path prints the tail of that file.
//! - **The artifact is checked by this side.** `holosmith build` reads the ELF header of the bytes that landed:
//!   `e_machine` for the triple that was asked for, `ET_EXEC`, and no `PT_INTERP` — the three properties
//!   `scripts/check-static.sh` asserts with `file` and `readelf`, read here without either program.
//!
//! The build is *safe*, never *trusted*: item 12's diff review is the real gate, and it happens before any of this
//! runs (ADR-0080).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::docker::Docker;
use crate::images;
use crate::remote::Machine;

/// The builder recipe, as the file in `crates/holosmith/images/`. Embedded for `images/`'s reason: a build runs on
/// a machine that has the artifact and nothing else, so the recipe has to be inside it.
pub const DOCKERFILE: &str = include_str!("../images/builder.Dockerfile");

/// What the container runs, embedded for the same reason.
pub const ENTRY: &str = include_str!("../images/builder-entry.sh");

/// The names the two files take **inside the build context**, which is the one place their names are not the
/// repository's: the daemon reads a context's `Dockerfile` and nothing else, and `COPY` resolves against the
/// context, not against `images/`.
const DOCKERFILE_IN_CONTEXT: &str = "Dockerfile";
const ENTRY_IN_CONTEXT: &str = "build-entry.sh";

/// The label that says which recipe an image was built from.
///
/// A version tag cannot answer that question: editing the Dockerfile or the entry script without bumping
/// `Cargo.toml` would leave the same tag over a different image, and every later build would silently use the old
/// toolchains. The value is the SHA-256 of both files, so any edit to either one is a rebuild.
pub const LABEL_RECIPE: &str = "org.holosmith.builder-recipe-sha256";

/// The image's name, tagged with this binary's version like the two product images.
pub const IMAGE_NAME: &str = "holosmith-builder";

/// One image build's bound. Six minutes of downloads on a cold machine here, so half an hour is generous and still
/// a bound: `Ok(None)` is "it did not finish", which is a sentence rather than a hang.
pub const IMAGE_TIMEOUT: Duration = Duration::from_secs(1800);

/// One artifact build's bound: the web app, then a release compile of this crate and its dependencies for one
/// target. Ten minutes here on a cold container, so an hour is a bound and not a timeout to fail by.
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(3600);

/// How much of a failed build's output is printed: enough to hold cargo's error and the layer it came from, and
/// short enough to read on a phone. `exit.rs`'s five is a container's dying words; this is a compiler's.
pub const TAIL_LINES: usize = 40;

/// The artifact's name inside the output directory, and its digest's — item 10's `releases/<tag>/` holds these two
/// and `holosmith upgrade` reads them (HS-078). The directory *layout* and the `current` symlink are HS-080's.
pub const BINARY: &str = "holosmith";
pub const DIGEST: &str = "holosmith.sha256";

/// Where the container writes the artifact before this side has looked at it: beside the final name, so a build
/// that fails between the two leaves nothing that reads as a release.
pub const PENDING: &str = ".holosmith.new";

/// The directory a build lands in when `--out` says nothing: item 10's `releases/<tag>/`.
pub const RELEASES: &str = "releases";

/// The two targets `scripts/build.sh` builds and the spec names ("Targets: x86_64 and aarch64 Linux, static musl").
pub const X86_64: &str = "x86_64-unknown-linux-musl";
pub const AARCH64: &str = "aarch64-unknown-linux-musl";

/// `holosmith-builder:<version>`: the image a release build runs in.
pub fn image() -> String {
    format!("{IMAGE_NAME}:{}", env!("CARGO_PKG_VERSION"))
}

/// The recipe's digest: the SHA-256 of the Dockerfile and the entry script, which is what the image's label carries.
///
/// Both files, in this order, with a separator that cannot be forged by moving bytes from one to the other: a
/// recipe is a pair, and half of one is not a recipe.
pub fn recipe() -> String {
    images::hex(&images::sha256(
        format!("{DOCKERFILE}\n---\n{ENTRY}").as_bytes(),
    ))
}

/// The ustar context the daemon reads: the recipe under the two names the context has.
///
/// `images::archive`'s shape and its reason — the daemon may not share this process's filesystem, so a *path* is
/// not a context. It carries no binary, which is the one way this archive differs from the two product images':
/// those are built *from* this program, and this one builds it.
pub fn archive() -> Vec<u8> {
    let mut out = Vec::new();
    images::push_entry(
        &mut out,
        DOCKERFILE_IN_CONTEXT,
        0o644,
        DOCKERFILE.as_bytes(),
    );
    images::push_entry(&mut out, ENTRY_IN_CONTEXT, 0o644, ENTRY.as_bytes());
    out.resize(out.len() + 1024, 0);
    out
}

/// `docker build -t <image> --build-arg HOLOSMITH_BUILDER_RECIPE=<recipe> -`: the context is the archive on stdin.
pub fn build_arguments(image: &str) -> Vec<String> {
    vec![
        "build".to_string(),
        "-t".to_string(),
        image.to_string(),
        "--build-arg".to_string(),
        format!("HOLOSMITH_BUILDER_RECIPE={}", recipe()),
        "-".to_string(),
    ]
}

/// The one `docker run` vector a build is.
///
/// Five decisions are in here:
///
/// - `--rm`: the container is one build and leaves nothing behind, so a failed run cannot leave a name that blocks
///   the next one.
/// - `--user <uid>:<gid>` is the **output directory's owner**, taken from the filesystem rather than from a
///   `getuid` this build does not have. Everything the build writes into `/out` then belongs to the operator, and
///   the container's own root is not the user the build runs as.
/// - `-v <source>:/src:ro` is read-only, so an agent-authored `build.rs` cannot write to the tree it is built from.
/// - `-v <out>:/out` is the only host path the build may write, and it is where the artifact is asked for.
/// - `-e HOLOSMITH_GIT_TAG=<tag>` is `scripts/build.sh`'s hatch: the container has no `.git` (the copy leaves it
///   behind), so the tag the source tree has arrives as a variable, and the artifact reports the tree it came from
///   rather than `unknown`.
pub fn run_arguments(
    image: &str,
    source: &Path,
    out: &Path,
    tag: &str,
    target: &str,
    owner: (u32, u32),
) -> Vec<String> {
    vec![
        "run".to_string(),
        "--rm".to_string(),
        "--user".to_string(),
        format!("{}:{}", owner.0, owner.1),
        "-v".to_string(),
        format!("{}:/src:ro", source.display()),
        "-v".to_string(),
        format!("{}:/out", out.display()),
        "-e".to_string(),
        format!("HOLOSMITH_GIT_TAG={tag}"),
        image.to_string(),
        target.to_string(),
    ]
}

/// The last `lines` lines of a build's output, without the trailing blank ones.
///
/// The tail and not the head: a build's first lines are the daemon's progress, and its last lines are the compiler
/// saying what is wrong — which is the only part an operator acts on.
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n").trim_end().to_string()
}

/// What the output directory already held, from [`place`]'s point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// Nothing was there: the artifact was moved into place and its digest written beside it.
    Fresh,
    /// The directory already held exactly these bytes: nothing was written, and the pending file is gone.
    Current,
    /// The directory holds a **different** binary under the same name: refused, both digests named, nothing
    /// replaced.
    ///
    /// Overwriting would leave one release directory holding two different binaries for one tag — and `--dirty`
    /// gives a changed tree the same tag as the tree it was built from, so this is the ordinary case, not a
    /// hypothetical one.
    Different { existing: String, built: String },
}

/// Put the built artifact where `holosmith upgrade` will look for it, or say why not.
///
/// `out` holds [`PENDING`] (what the container wrote) and may hold [`BINARY`] (what a previous run placed). The
/// three outcomes are [`Placement`]'s, and the pending file never survives: a refusal that left it behind would
/// make the next run's question "is this the build I just made" unanswerable.
pub fn place(out: &Path, built: &str) -> Result<Placement, String> {
    let pending = out.join(PENDING);
    let artifact = out.join(BINARY);
    let placement = if artifact.is_file() {
        let existing = images::digest(&artifact)?;
        if existing == built {
            // The digest file is written here too when it is missing: a directory can hold the right bytes without
            // it — a run whose rename succeeded and whose write failed leaves exactly that — and `upgrade` reads two
            // files, so "already this build" must mean both are there.
            write_digest(out, built)?;
            Placement::Current
        } else {
            Placement::Different {
                existing,
                built: built.to_string(),
            }
        }
    } else {
        std::fs::rename(&pending, &artifact).map_err(|error| {
            format!(
                "the build's artifact could not be moved from {} to {} ({error})",
                pending.display(),
                artifact.display()
            )
        })?;
        write_digest(out, built)?;
        Placement::Fresh
    };
    let _ = std::fs::remove_file(&pending);
    Ok(placement)
}

/// Write `<out>/holosmith.sha256` in the format `sha256sum -c` reads: two spaces between the value and the name,
/// because that is the file an operator checks with the tool they already have.
fn write_digest(out: &Path, built: &str) -> Result<(), String> {
    let path = out.join(DIGEST);
    std::fs::write(&path, format!("{built}  {BINARY}\n")).map_err(|error| {
        format!(
            "the artifact is at {} but its digest could not be written to {} ({error})",
            out.join(BINARY).display(),
            path.display()
        )
    })
}

/// The two properties of a release artifact this side can read without `file` or `readelf`: the machine it is for,
/// and that it needs no dynamic loader.
///
/// `e_machine` is what the file *is*, not what its name says (ADR-0078 reads it for the same reason), and the
/// interpreter segment is what a glibc or static-PIE build has: `scripts/check-static.sh` asserts both with
/// `readelf`, and this reads them from the bytes so the command can refuse its own output before placing it.
pub fn artifact_problem(path: &Path, bytes: &[u8], target: &str) -> Option<String> {
    let wanted = Machine::parse(target.split('-').next().unwrap_or(target));
    let machine = match Machine::of_elf(elf_field(bytes, 18).unwrap_or_default()) {
        Some(machine) => machine,
        None => {
            return Some(format!(
                "the build produced {} but it is not an ELF binary for a machine this build names: a release is a \
                 static Linux binary for {X86_64} or {AARCH64}",
                path.display()
            ));
        }
    };
    if let Some(wanted) = wanted
        && wanted != machine
    {
        return Some(format!(
            "the build produced {} for {}, and {} was asked for: the container's toolchain did not build the \
             target this command names",
            path.display(),
            machine.word(),
            target
        ));
    }
    // `e_type` 2 is `ET_EXEC`; 3 is `ET_DYN`, which is what a static-PIE build is and what ADR-0005 refuses.
    if elf_field(bytes, 16) != Some(2) {
        return Some(format!(
            "the build produced {} but it is not an executable (ELF type {:?}, and 2 is what a static build is): \
             a static-PIE artifact is what an exported RUSTFLAGS produces (ADR-0005)",
            path.display(),
            elf_field(bytes, 16)
        ));
    }
    if interpreter_segment(bytes).is_some() {
        return Some(format!(
            "the build produced {} with a PT_INTERP segment, so it needs a dynamic loader: a release artifact is \
             statically linked, and this one would not run on a machine without the loader it names",
            path.display()
        ));
    }
    None
}

/// A little-endian `u16` at `offset`, which is the only field width these checks need.
fn elf_field(bytes: &[u8], offset: usize) -> Option<u16> {
    let field = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([field[0], field[1]]))
}

/// A `u64` at `offset`, for the program header table's position.
fn elf_word(bytes: &[u8], offset: usize) -> Option<u64> {
    let field = bytes.get(offset..offset + 8)?;
    let mut value = [0_u8; 8];
    value.copy_from_slice(field);
    Some(u64::from_le_bytes(value))
}

/// The first `PT_INTERP` program header's type, or `None` when there is none.
///
/// `PT_INTERP` is 3. `e_phoff`, `e_phentsize` and `e_phnum` are at 32, 54 and 56 in the 64-bit header, and every
/// entry starts with its 4-byte `p_type` — which is the whole of what this needs to know.
fn interpreter_segment(bytes: &[u8]) -> Option<u32> {
    let table = elf_word(bytes, 32)? as usize;
    let size = elf_field(bytes, 54)? as usize;
    let count = elf_field(bytes, 56)? as usize;
    if size < 4 {
        return None;
    }
    for index in 0..count {
        let at = table.checked_add(index.checked_mul(size)?)?;
        let kind = bytes.get(at..at + 4)?;
        let kind = u32::from_le_bytes([kind[0], kind[1], kind[2], kind[3]]);
        if kind == 3 {
            return Some(kind);
        }
    }
    None
}

/// The tag the mounted tree has, as `scripts/build.sh` computes it: `git describe --tags --always --dirty`, and
/// `unknown` for a tree with no history.
///
/// The host runs this and the container does not: it is metadata about the source, not a build step, and a copy of
/// the tree without `.git` cannot describe itself. `scripts/build.sh` honours the value as `HOLOSMITH_GIT_TAG`.
pub fn tag(source: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|tag| !tag.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Whether `source` is a Holosmith tree at all: the two files the container's entry script needs.
///
/// Before anything is mounted or built, because "the wrong directory was mounted" is a mistake with a cheap
/// sentence and an expensive symptom — a container that fails a minute later inside `tar`.
pub fn source_problem(source: &Path) -> Option<String> {
    let recipe = source.join("scripts").join("build.sh");
    let workspace = source.join("Cargo.toml");
    if workspace.is_file() && recipe.is_file() {
        return None;
    }
    Some(format!(
        "{} is not a Holosmith source tree: a build mounts the current directory at /src and runs its \
         scripts/build.sh, and {} is missing",
        source.display(),
        if workspace.is_file() {
            "scripts/build.sh"
        } else {
            "Cargo.toml"
        }
    ))
}

/// The target triple a machine word names, for `--target`'s default.
pub fn target_of(word: &str) -> Option<&'static str> {
    match Machine::parse(word)? {
        Machine::X86_64 => Some(X86_64),
        Machine::Aarch64 => Some(AARCH64),
    }
}

/// `--target`'s default: the triple this binary was built for, which is the machine it runs on.
///
/// `HOLOSMITH_TARGET`'s first field is `build.rs`'s `TARGET`, so a debug build (`…-unknown-linux-gnu`) answers the
/// same word a release build does — the architecture, not the libc, which is what the target triple's first field
/// is. A binary built for something else entirely is refused rather than guessed at.
pub fn default_target() -> Result<&'static str, String> {
    let word = env!("HOLOSMITH_TARGET");
    let prefix = word.split('-').next().unwrap_or(word);
    target_of(prefix).ok_or_else(|| {
        format!(
            "this binary is built for {word}, which is not one of the two release targets ({X86_64}, {AARCH64}): \
             `--target` names the artifact to build"
        )
    })
}

/// The output directory, made absolute: `docker -v` resolves a relative host path against the daemon's own working
/// directory, which is not this process's.
pub fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|error| {
        format!(
            "the output directory {} could not be resolved to an absolute path ({error}): a build mounts it into \
             the container, and a relative path would be the daemon's",
            path.display()
        )
    })
}

/// The owner of a directory, as `--user` takes it: the operator's own uid and gid, read from the filesystem because
/// this build has no `getuid`.
pub fn owner(path: &Path) -> Result<(u32, u32), String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "the output directory {} could not be read ({error}): the container runs as its owner, so that the \
             artifact belongs to you",
            path.display()
        )
    })?;
    Ok((metadata.uid(), metadata.gid()))
}

/// Ask the daemon who it is, before anything is built: no daemon is a sentence, not a half-run build.
pub fn daemon_problem(docker: &Docker) -> Option<String> {
    match docker.run(&["version", "--format", "{{.Server.Arch}}"]) {
        Ok(arch) if !arch.trim().is_empty() => None,
        Ok(_) => Some(
            "this machine's Docker daemon answered nothing about itself, so no build ran: a build runs in a \
             container, and `holosmith doctor --quick` says which check fails"
                .to_string(),
        ),
        Err(said) => Some(format!(
            "this machine's Docker daemon did not answer ({said}), so no build ran: a release build runs in a \
             container and the host only orchestrates it — start the daemon, or point `docker_host` at one"
        )),
    }
}

/// Ensure the builder image is there and built from this recipe, answering the line to print.
///
/// The image is **built locally, never pulled**: it is this program's own recipe, and a registry would be a second
/// supply chain for the one step that must not run foreign code. A cold machine needs the network for the two bases,
/// the musl.cc tarballs, cargo's registry and npm's — which is why a failure here is a sentence about the network
/// rather than a wrong Dockerfile.
pub fn ensure_image(docker: &Docker) -> Result<String, String> {
    let image = image();
    let wanted = recipe();
    let label = format!("{{{{index .Config.Labels \"{LABEL_RECIPE}\"}}}}");
    if let Ok(found) = docker.image_inspect(&image, &label)
        && found.trim() == wanted
    {
        return Ok(format!("{image} is here, built from this recipe"));
    }
    let context = archive();
    let arguments = build_arguments(&image);
    let log = docker
        .run_captured(&arguments, Some(&context), IMAGE_TIMEOUT)
        .map_err(|error| error.to_string())?;
    match log {
        None => Err(format!(
            "the builder image {image} did not finish building within {}s and was killed: a cold image fetches \
             both bases, the two musl toolchains and the rust targets over the network, so this is a machine that \
             could not reach it rather than a recipe that is wrong",
            IMAGE_TIMEOUT.as_secs()
        )),
        Some(log) if !log.success => Err(images::failure(&image, &log.text)),
        Some(_) => Ok(format!("{image} was built from this recipe")),
    }
}

/// Run the container for one target, answering the artifact's bytes' tail of output — or the reason there is none.
pub fn run(
    docker: &Docker,
    image: &str,
    source: &Path,
    out: &Path,
    tag: &str,
    target: &str,
    owner: (u32, u32),
) -> Result<(), String> {
    let arguments = run_arguments(image, source, out, tag, target, owner);
    let log = docker
        .run_captured(&arguments, None, BUILD_TIMEOUT)
        .map_err(|error| error.to_string())?;
    match log {
        None => Err(format!(
            "the build of {target} did not finish within {}s and was killed: the container was still running, so \
             nothing was placed",
            BUILD_TIMEOUT.as_secs()
        )),
        Some(log) if !log.success => Err(format!(
            "the build of {target} failed in {image}, and its last {TAIL_LINES} lines are:\n{}",
            tail(&log.text, TAIL_LINES)
        )),
        Some(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recipe_pins_both_bases_by_digest() {
        let froms: Vec<&str> = DOCKERFILE
            .lines()
            .filter(|line| line.trim_start().starts_with("FROM "))
            .collect();
        assert_eq!(froms.len(), 2, "the recipe has two bases: {froms:?}");
        for line in &froms {
            let reference = line.split_whitespace().nth(1).unwrap_or_default();
            assert!(
                reference.contains("@sha256:"),
                "a base is pinned by its digest, not its tag: {line}"
            );
            let digest = reference.rsplit("@sha256:").next().unwrap_or_default();
            assert_eq!(digest.len(), 64, "a digest is 64 hex characters: {line}");
        }
    }

    #[test]
    fn every_copy_from_names_a_stage_this_recipe_declares() {
        // `COPY --from=rust` resolves against stage names first and an image reference second, so an unnamed `FROM`
        // stage silently becomes `rust:latest` — an extra unpinned pull, and a toolchain nobody pinned. Measured in
        // this story's review: the build loaded `docker.io/library/rust:latest` beside the pinned base.
        let stages: Vec<&str> = DOCKERFILE
            .lines()
            .filter_map(|line| line.trim().split_once(" AS "))
            .map(|(_, name)| name.trim())
            .collect();
        assert!(!stages.is_empty(), "the recipe declares its stages");
        let copies: Vec<&str> = DOCKERFILE
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("COPY --from="))
            .collect();
        assert!(!copies.is_empty(), "the recipe copies from its stages");
        for line in copies {
            let name = line
                .trim_start_matches("COPY --from=")
                .split_whitespace()
                .next()
                .unwrap_or_default();
            assert!(
                stages.contains(&name),
                "COPY --from={name} names no stage of this recipe (stages: {stages:?}): it would be an image \
                 reference, and an unpinned one"
            );
        }
    }

    #[test]
    fn the_two_musl_toolchains_are_checked_against_their_digests() {
        let checks = DOCKERFILE.matches("| sha256sum -c -").count();
        assert_eq!(
            checks, 2,
            "each musl.cc download is checked against its digest"
        );
    }

    #[test]
    fn the_recipe_never_sets_rustflags() {
        // ADR-0005: an exported RUSTFLAGS disables `.cargo/config.toml`'s per-target rustflags and ships a
        // static-PIE binary, and a container is one more place that trap can be set. A *comment* naming the trap is
        // the point — an instruction setting it is the bug, so only non-comment lines are read.
        let setting: Vec<&str> = DOCKERFILE
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .filter(|line| line.contains("RUSTFLAGS"))
            .collect();
        assert!(
            setting.is_empty(),
            "the recipe must not set RUSTFLAGS (ADR-0005): {setting:?}"
        );
    }

    #[test]
    fn the_recipe_digest_covers_both_files() {
        let recipe = recipe();
        assert_eq!(recipe.len(), 64);
        assert_ne!(
            recipe,
            images::hex(&images::sha256(DOCKERFILE.as_bytes())),
            "the entry script is half the recipe: a digest of the Dockerfile alone would not rebuild when it changes"
        );
    }

    #[test]
    fn the_archive_holds_the_two_recipe_files_and_is_reproducible() {
        let context = archive();
        assert_eq!(
            context,
            archive(),
            "the same recipe must produce the same context"
        );
        let text = String::from_utf8_lossy(&context);
        assert!(text.contains("Dockerfile"));
        assert!(text.contains("build-entry.sh"));
        assert!(
            context.ends_with(&[0_u8; 1024]),
            "a tar archive ends with two zero blocks"
        );
    }

    #[test]
    fn the_build_arguments_name_the_recipe_and_the_context() {
        let arguments = build_arguments("holosmith-builder:9.9.9");
        assert_eq!(
            arguments,
            vec![
                "build".to_string(),
                "-t".to_string(),
                "holosmith-builder:9.9.9".to_string(),
                "--build-arg".to_string(),
                format!("HOLOSMITH_BUILDER_RECIPE={}", recipe()),
                "-".to_string(),
            ]
        );
    }

    #[test]
    fn the_run_arguments_mount_the_source_read_only_and_the_output_writable() {
        let arguments = run_arguments(
            "holosmith-builder:9.9.9",
            Path::new("/src/tree"),
            Path::new("/out/dir"),
            "v1-2-gabc",
            X86_64,
            (1000, 1001),
        );
        assert_eq!(
            arguments,
            vec![
                "run".to_string(),
                "--rm".to_string(),
                "--user".to_string(),
                "1000:1001".to_string(),
                "-v".to_string(),
                "/src/tree:/src:ro".to_string(),
                "-v".to_string(),
                "/out/dir:/out".to_string(),
                "-e".to_string(),
                "HOLOSMITH_GIT_TAG=v1-2-gabc".to_string(),
                "holosmith-builder:9.9.9".to_string(),
                X86_64.to_string(),
            ]
        );
    }

    #[test]
    fn the_tail_is_the_end_of_the_output() {
        let text: String = (1..=60).map(|n| format!("line {n}\n")).collect();
        let tailed = tail(&text, TAIL_LINES);
        assert!(tailed.starts_with("line 21\n"));
        assert!(tailed.ends_with("line 60"));
        assert_eq!(tailed.lines().count(), TAIL_LINES);
        assert_eq!(tail("only one\n\n", 5), "only one");
        assert_eq!(tail("", 5), "");
    }
}
