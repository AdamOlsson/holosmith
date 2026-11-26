# The rust base is a pin file, and the recipes read it as a build argument

Both recipes that carry a Rust toolchain named their base **literally**:
`crates/holosmith/images/agent.Dockerfile` and `crates/holosmith/images/builder.Dockerfile` each held the identical
`FROM rust:` line, and `tests/pin_single_source.rs` existed to keep the two strings equal and to keep the digest out of
every third file. That worked for two readers.

`scripts/build.sh` is now a third. An artifact compiled by a rustc that is not the one the image runs is not the
artifact that image would have produced, so the script refuses a host whose cargo is not the pin — and it cannot read
a `FROM` line's digest out of a Dockerfile in shell without either a `sed` that repeats the shape or a copy of the
version. A rule that holds two literals together has no answer for a reader that is not a Dockerfile.

## D1 — `pins/rust.txt` holds the whole reference, and `pins::rust_image()` is its only reader

One line, the shape `pins/pi.txt` already has: `pins::rust_image()` is `include_str!` of it, trimmed, and `build.rs`
gains `cargo:rerun-if-changed` for it — the same pair the pi pin has (ADR-0113 is the same argument for `VERSION`).

**The whole reference — tag and digest — and not just the version.** Docker resolves `rust:tag@sha256:…` by the
**digest** and ignores the tag. A pin holding only the version would therefore build from whatever digest the
recipes were left with while claiming a version nothing checked, and the one thing a pin exists to prevent — a build
from bytes nobody vetted — would be the thing the pin failed to notice. One value in one file cannot drift from
itself.

`Cargo.toml`'s `rust-version` is a **different fact** and stays where it is: a floor cargo refuses to build below,
which says nothing about above. The test ties its `major.minor` to the pin's, so the floor cannot drift from the
version an image actually holds, and a patch bump in the pin needs no manifest edit.

## D2 — the recipes take it as an argument, and nothing else in the tree spells it

Each recipe declares the argument **before its first `FROM`** — the only scope a `FROM` can read an argument from —
and writes the stage in one spelling:

```text
ARG HOLOSMITH_RUST_IMAGE
FROM rust:${HOLOSMITH_RUST_IMAGE} AS rust
```

The evaluator passes the file's value: `images::build_arguments` for the agent image and `builder::build_arguments`
for the builder, both reading `pins::rust_image()`. It is **not** a member of `images::Expectation`, because that
struct is the four label facts and a test asserts its vector exactly, and it is not sent to the proxy, base or smoke
recipes — none of them declares a rust stage, and a `--build-arg` no instruction reads is a BuildKit warning over
nothing.

ADR-0105's D2 refused exactly this shape on two grounds, and both are answered rather than overruled:

- **"A build argument would move the pin into the Rust that passes it."** It moves the *file's* value, and nothing
  else: no version, no digest and no reference is written in any `.rs` file, which
  `no_tracked_file_beside_the_pin_holds_the_rust_base_digest` asserts over every tracked file. A second copy is a
  literal; a value read from the one file is the file.
- **"It would be a config key."** It is not one. No `Config` field, no default, no label and no environment variable
  reaches it: the argument's value is `pins::rust_image()`'s and a test asserts the vector carries that and nothing a
  machine could set. What D1 refused — a machine moving the toolchain — is still refused.

## D3 — the argument above the first `FROM` costs no cache boundary

ADR-0115 is about a label-only argument declared **last**, below every `RUN`, because a `RUN` is in scope of every
argument above it and BuildKit puts that scope in the cache key. This argument is in the opposite place for the
opposite reason: a `FROM` reads only global scope. It costs nothing, because a stage's base reference is above every
layer — changing it always rebuilt the stage and everything after it, exactly as editing the literal did.

## Consequences

- **A bump within the same minor is one edit**: `pins/rust.txt`, and it holds the digest itself. The whole reference
  — tag and digest — lives in that one file. A **minor** bump is more, and this change deliberately leaves the rest
  alone: `Cargo.toml`'s `rust-version` moves with it (the floor test forces that), and so do `scripts/gate-quick.sh`'s
  `1.99.*` match and the three sentences around it, and `scripts/verify.sh`'s stub expectation of the footer that
  match produces. Pointing `gate-quick.sh` at `pins/rust.txt` the way `scripts/build.sh` reads it is the follow-up.
  The `1.99+` floor that `AGENTS.md`, `README.md` and `docs/requirements.md` name stays true through a minor bump —
  it is a minimum, not the pin. `tests/pin_single_source.rs` reads the recipes from a list, so the builder recipe's
  deletion is one line dropped from it rather than a rewrite of the rule.
- **`scripts/build.sh` reads the version with one `tr`** and refuses a cargo that is not it, which makes it stricter
  than `scripts/gate-quick.sh` on purpose: that gate compares the `major.minor` line only, so a patch difference is
  caught by the build and nowhere else. The two-line derivation is the same two lines `case_agent_toolchain` runs,
  and that is deliberate: `scripts/build.sh` sources no file of this repository today — the one file it does source
  is the machine-local toolchain env, and only when a linker is missing — so a shared reader would be a dependency
  on another tracked file taken before it reads its first value. It is worth a helper when a third script needs the
  version, or when `build.sh` already sources a repository file for another reason. `case_agent_toolchain` reads the
  same file rather than the recipe's `FROM` line.
- **The image still builds with no fetch beyond its own**: the argument carries the base's full reference, so the
  pull is the pinned one and the daemon resolves nothing new.
