//! Where a release artifact lands, and what a candidate for one must be.
//!
//! There is no recipe in here and no image: a release artifact is the tree's own `scripts/build.sh`'s output, built
//! where that script runs, and this module only judges the bytes it is handed (ADR-0121). Two questions, both about
//! a *file*:
//!
//! - **Where it lands.** [`place`] states the rule a release directory obeys: a build's bytes move in under
//!   [`BINARY`], a *different* binary under that name is refused rather than replaced, and [`DIGEST`] is written beside
//!   it. The one **writer** of a release directory is [`crate::release::Release::write`], which implements the same
//!   three outcomes — [`Placement`] names them — so [`place`] is that rule stated and not a step anything calls: its
//!   only callers are `tests/build.rs`. [`BINARY`], [`DIGEST`] and [`PENDING`] are the names `crate::pass`,
//!   [`crate::promote`] and `crate::release` read.
//! - **What it must be.** [`artifact_problem`] reads an ELF header for the machine it is for and the interpreter it
//!   must not name — `e_machine` for the triple that was asked for, `ET_EXEC`, and no `PT_INTERP`, the three
//!   properties `scripts/check-static.sh` asserts with `file` and `readelf` — and [`default_target`] names the triple
//!   this binary was built for. `holosmith smoke` asks both of the `--binary` a promotion is about to make live, and
//!   `promote` asks [`artifact_problem`] of the candidate it is handing the daemon.
//!
//! Two container helpers sit here because `holosmith smoke` needs them: [`daemon_problem`], the one sentence for a
//! machine whose daemon did not answer, and [`tail`], the end of a child's output rather than its head.
//!
//! The bytes are *checked*, never *trusted*: item 12's diff review is the real gate, and it happens before any of this
//! runs (ADR-0080).

use std::path::Path;

use crate::docker::Docker;
use crate::images;
use crate::remote::Machine;

/// The artifact's name inside the output directory, and its digest's — item 10's `releases/<tag>/` holds these two
/// and `holosmith upgrade` reads them. The directory *layout* and the `current` symlink are that story's.
pub const BINARY: &str = "holosmith";
pub const DIGEST: &str = "holosmith.sha256";

/// Where a build writes the artifact before this side has looked at it: beside the final name, so a build that fails
/// between the two leaves nothing that reads as a release.
pub const PENDING: &str = ".holosmith.new";

/// The two release triples the artifact checks can describe, though `scripts/build.sh` builds only this machine's
/// (ADR-0119): a hand-built aarch64 artifact is still something `--binary` may be given.
pub const X86_64: &str = "x86_64-unknown-linux-musl";
pub const AARCH64: &str = "aarch64-unknown-linux-musl";

/// How much of a child's output is printed when a caller has no count of its own: enough to hold a compiler's error
/// and the layer it came from, and short enough to read on a phone. `holosmith smoke` passes [`tail`] its own, smaller
/// count, because a candidate's log is not a build's.
pub const TAIL_LINES: usize = 40;

/// The last `lines` lines of a child's output, without the trailing blank ones.
///
/// The tail and not the head: a container's first lines are the daemon's progress, and its last lines say what went
/// wrong — which is the only part an operator acts on. `holosmith smoke` prints one above a failed measurement's
/// sentence.
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
/// `out` holds [`PENDING`] (what the build wrote) and may hold [`BINARY`] (what a previous run placed). The
/// three outcomes are [`Placement`]'s, and the pending file never survives: a refusal that left it behind would
/// make the next run's question "is this the build I just made" unanswerable.
///
/// The rule rather than the producer: [`crate::release::Release::write`] is what writes a release directory in this
/// tree, and it implements these three outcomes. This function is the statement of them, and `tests/build.rs` is what
/// runs it.
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
///
/// Each sentence names the **file**, not a build: two callers ask this question — `holosmith smoke` about the
/// `--binary` a promotion is about to make live, and the promotion itself about the candidate it is handing the
/// daemon — and only one of them talked to a container.
pub fn artifact_problem(path: &Path, bytes: &[u8], target: &str) -> Option<String> {
    let wanted = Machine::parse(target.split('-').next().unwrap_or(target));
    let machine = match Machine::of_elf(elf_field(bytes, 18).unwrap_or_default()) {
        Some(machine) => machine,
        None => {
            return Some(format!(
                "{} is not an ELF binary for a machine this build names: a release is a static Linux binary for \
                 {X86_64} or {AARCH64}",
                path.display()
            ));
        }
    };
    if let Some(wanted) = wanted
        && wanted != machine
    {
        return Some(format!(
            "{} is an artifact for {}, and {} was asked for: the toolchain that produced it did not build the target \
             this command names",
            path.display(),
            machine.word(),
            target
        ));
    }
    // `e_type` 2 is `ET_EXEC`; 3 is `ET_DYN`, which is what a static-PIE build is and what ADR-0005 refuses.
    if elf_field(bytes, 16) != Some(2) {
        return Some(format!(
            "{} is not an executable (ELF type {:?}, and 2 is what a static build is): a static-PIE artifact is \
             what an exported RUSTFLAGS produces (ADR-0005)",
            path.display(),
            elf_field(bytes, 16)
        ));
    }
    if interpreter_segment(bytes).is_some() {
        return Some(format!(
            "{} has a PT_INTERP segment, so it needs a dynamic loader: a release artifact is statically linked, and \
             this one would not run on a machine without the loader it names",
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

/// The target triple a machine word names — the mapping [`default_target`] answers with.
pub fn target_of(word: &str) -> Option<&'static str> {
    match Machine::parse(word)? {
        Machine::X86_64 => Some(X86_64),
        Machine::Aarch64 => Some(AARCH64),
    }
}

/// The triple this binary was built for, which is the machine it runs on — the one a candidate has to be.
///
/// `crate::pass`'s candidate read and `crate::promote`'s artifact check are the callers. `HOLOSMITH_TARGET`'s first
/// field is `build.rs`'s `TARGET`, so a debug build (`…-unknown-linux-gnu`) answers the same word a release build
/// does — the architecture, not the libc, which is what the target triple's first field is. A binary built for
/// something else entirely is refused rather than guessed at.
pub fn default_target() -> Result<&'static str, String> {
    let word = env!("HOLOSMITH_TARGET");
    let prefix = word.split('-').next().unwrap_or(word);
    target_of(prefix).ok_or_else(|| {
        format!(
            "this binary is built for {word}, which is not one of the two release targets ({X86_64}, {AARCH64}): a \
             candidate for `smoke --binary` or for a promotion has to be one of them"
        )
    })
}

/// Ask the daemon who it is, before anything is started: no daemon is a sentence, not a half-run smoke test.
///
/// `holosmith smoke` is the caller, and its shape explains the words: a smoke run builds its own image and starts a
/// session's container, so a machine with no daemon has a measurement it could not make rather than a candidate that
/// failed it. `Ok(_)` with an empty answer is a daemon that answered nothing about itself, which is the same ending.
pub fn daemon_problem(docker: &Docker) -> Option<String> {
    match docker.run(&["version", "--format", "{{.Server.Arch}}"]) {
        Ok(arch) if !arch.trim().is_empty() => None,
        Ok(_) => Some(
            "this machine's Docker daemon answered nothing about itself, so nothing was started: a smoke run \
             builds its own image and runs a session's container, and `holosmith doctor --quick` says which check \
             fails"
                .to_string(),
        ),
        Err(said) => Some(format!(
            "this machine's Docker daemon did not answer ({said}), so nothing was started: a smoke run builds its \
             own image and runs a session's container — start the daemon, or point `docker_host` at one"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
