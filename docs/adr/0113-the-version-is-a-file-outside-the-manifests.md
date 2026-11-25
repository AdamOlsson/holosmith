# The version is a file of its own, outside the manifests

The version is `VERSION` at the repository root and nowhere else. `crates/holosmith/build.rs` reads that file into
`holosmith::VERSION`, which is what `version`, `/api/health`, the release directory and every image tag report; both
manifests hold `0.0.0` as a placeholder that nothing may read.

The reason is a promotion's precondition. `scripts/check-crate-set.sh` digests `Cargo.lock` and compares it with the
digest the agent image baked into `/opt/cargo-vendor/.holosmith-cargo-lock`, so that a session's `cargo build` failing
with `the crate X could not be found in registry index` reads as the stale image it is. A version living in
`[workspace.package]` lands in that lock, so it moves that digest while the crate set stands still: a tree that bumped
`0.1.0` to `0.1.1` and changed nothing else answers exit **3** at the gate's preflight, and the promotion refuses
before its first phase. Measured on this repository: the only hunk in that lock between the two releases was
`name = "holosmith"`'s own `version` line.

Bumping the patch on almost every commit is a workflow this tree should support, so the fix is to stop putting the
version where a precondition digests.

## D1 — `VERSION` is the one copy, and `build.rs` is the one reader

`build.rs` reads `../../VERSION`, trims it, and refuses the build on two shapes: no file, and one that does not hold
exactly one token. A newline would reach `version --json` and `agent-image:<version>`; a missing file is a source copy
that dropped it. Both are `cargo::error=`, so they stop the build rather than producing a binary that reports a version
nothing agrees with.

The value reaches code as `holosmith::VERSION`, from `cargo:rustc-env=HOLOSMITH_VERSION`, the mechanism the git tag
already uses — and the same name the agent recipe takes as a build argument, because it is the same fact in a third
direction.

**`Cargo.toml` and `web/package.json` hold `0.0.0`.** Cargo and npm both require a version and neither needs the real
one. The placeholder is written with its reason beside it, because a `0.0.0` that looks like a mistake is one the next
reader will "fix".

`tests/version_single_source.rs` is what keeps the rule: the file is one line and equals the constant; both manifests
hold the placeholder; the lock's `holosmith` block names it; and no tracked `.rs` file reads the cargo package version,
which would answer the placeholder.

## D2 — a bump edits one file, so the fixtures substitute it

Two fixtures would otherwise carry the number and have to be edited by every bump: the golden default config, whose
rendered `agent_image` tag ends in the version, and its two readers. They spell it `<version>` and substitute
`holosmith::VERSION`, the same shape they already use for `<root>`. This is not tidiness: a bump that has to touch a
fixture is a bump that can be forgotten, and a forgotten one is a red gate for a change whose only edit was a number.

`scripts/verify-in-container.sh` read the tag out of `Cargo.toml`; it reads `VERSION` now, which is also what the
`HOLOSMITH_VERIFY_IMAGE` default means.

## D3 — what this does not decide

The three deeper changes are left open, and this ADR is deliberately narrower than the problem:

- **The precondition still digests a file, not a crate set.** A dependency edit still changes the digest and still
  refuses, which is correct — but the digest also moves for anything else in the lock, and the crate set's own identity
  is what the sentence actually describes.
- **A promotion still builds the agent image from the *running* binary's embedded context** (`images::AGENT_CONTEXT`,
  `include_str!`), not the candidate's, and no label covers it. So a promoted release whose crate set really moved
  ships an image vendoring the previous set, and the marker drifts one release behind. Version bumps are unaffected —
  the vendored crates are identical, and the promotion's build is a re-tag of cached layers — but a minor bump that
  also upgrades a dependency is not.
- **The image tag is still the release version**, so every bump names four new images (session, proxy, smoke, builder).
  They build from cache, which is cheap and not free; a content-addressed tag is the change that would make it free.

Each of those is its own decision with its own cost, and none of them is needed for a bump to be promotable.

## What is not measured

- **A promotion of a bump, end to end.** This commit changes `Cargo.lock` once — `0.1.1` to `0.0.0` — so the first
  promotion after it still meets the stale image the old digest named, and the repair is one image rebuild. A run that
  promotes a bump and shows the gate green is the parent's gate run plus one `server deploy`; no case in the suite can
  take a promotion all the way through a rebuild on a machine this tree does not own.
- **`npm ci` against a frozen `package.json`.** The web gate is `npm ci && npm test && npm run build`; the version field
  is not a dependency, so nothing in `web/test/` measures it.

## Consequences

- A bump is one line in `VERSION`, and `Cargo.lock` stops moving with it: from this release on, the crate-set
  precondition cannot fire for a version change.
- `Cargo.toml`'s `[workspace.package] version` is `0.0.0` forever. `grep CARGO_PKG_VERSION` in `src/` answers nothing,
  which is what `tests/version_single_source.rs` asserts.
- `AGENTS.md`'s "Build settings that bite" carries the rule, `GLOSSARY.md`'s **Version** entry says where the value
  lives, and the two manifests and `build.rs` carry the reason beside the placeholder so the next reader does not undo
  it.
