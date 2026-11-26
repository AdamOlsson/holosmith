//! The release artifact's rules that need a filesystem or an ELF header: where it lands, and what a candidate must
//! be.
//!
//! What is *pure* is tested where it lives: `default_target`'s answer and `target_of`'s mapping at the end of this
//! file, beside the constants they read, and the tail in `src/builder.rs`'s own `mod tests`.
//! What is here is what only a real file can show: a directory that already holds a different binary, an artifact for
//! the wrong machine, and a directory that already holds exactly these bytes.

mod support;

use std::path::Path;

use holosmith::builder::{self, Placement};
use holosmith::images;
use support::TempDir;

/// A file's SHA-256, as a promotion computes it: `images::digest`, the one producer of a release digest.
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
