# The build runs in a pinned builder container, and the host only orchestrates Docker

This is spec item 10's first step — "Build in a pinned builder container, not on the host, so `build.rs` and proc
macros never run host-side. Review the diff first (item 12); that review is the real gate" — and the three **Build and
release** bullets it touches. Its subject is one sentence long and its implementation is six decisions, because
"never on the host" is a claim about *where a process runs*, and this repository has three processes that could run
it: a shell script, the binary, and the container.

The sentence this ADR keeps saying, because it is the story's own limit: **this makes the build safe, never
trusted.** A container bounds what an agent-authored `build.rs`, proc macro or Vite config can reach; it does not
judge what they do. Item 12's diff review is the gate, and it happens before any of this runs.

## D1 — the verb is `holosmith build`, a host command that orchestrates Docker

`holosmith build [--target <triple>] [--out DIR]`. It is the third arm of the story's own question ("what invokes the
build, and where it lives"):

- **`scripts/build.sh --container`** was refused because a script is not the product. The operator, and an agent in a
  worktree, run the binary; the `upgrade <binary>` is a subcommand of the same program, and a build path that
  only exists inside a checkout is a second entry point for the pipeline item 10 describes as one.
- **Both** — the script delegating to the binary — was refused as two spellings of one behaviour in one story, and
  because the script's own path is still needed (D6).
- The spec's **CLI by phase** table adds no phase-4 verb beyond `upgrade` and `rollback`, so adding one is a decision
  rather than a reading. It is recorded here, and the Commands table's `upgrade <binary>` still takes a binary: this
  verb is what produces it.

`scripts/build.sh` keeps its host path unchanged, apart from one fix D5 records. It is the developer's and the gate's
artifact; `holosmith build` is the release's.

## D2 — the builder image is ours, and it is pinned twice

`crates/holosmith/images/builder.Dockerfile`, `include_str!`ed like the two product recipes (ADR-0057) and built
locally on first use from a ustar archive on stdin — the same archive argument as: the daemon may not share this
process's filesystem, so a *path* is not a build context.

Why not a published image pinned by digest:

- **no single official image has what a Holosmith release needs.** It needs rustc with both musl targets, the two
  musl.cc cross toolchains (aarch64's has no Debian package, so a glibc cross gcc cannot link it), and node 24 for the
  web app — `scripts/build-web.sh` runs `npm test`, and node's own test runner needs 22.18+ for native TypeScript.
- **a registry is a second supply chain** for the one step that must not run foreign code.

What it is pinned by:

- **two bases by manifest-list digest** — `node:24-bookworm-slim` (the same digest `agent.Dockerfile` names, and a test
  asserts the two agree) and `rust:1.99.1-bookworm`. One Dockerfile then resolves to the machine's own architecture,
  the rule ADR-0057 already records.
- **the two musl.cc tarballs by SHA-256**, checked with `sha256sum -c` in the `RUN` line. musl.cc is not
  content-addressed, so the checksum *is* the pin; a moved tarball is a build failure with a name, not a silent
  toolchain change.
- **the recipe by its own digest**, as a label: `org.holosmith.builder-recipe-sha256` is the SHA-256 of the Dockerfile
  and the entry script, and `holosmith build` rebuilds when the image's label is not that value. A version tag cannot
  answer the question — editing the recipe without bumping `Cargo.toml` would leave the same tag over different
  toolchains, which is the mistake `org.holosmith.binary-sha256` exists to prevent on the product images.

Two details of the recipe are load-bearing and each was a defect the review found or the first build measured:

- **both stages are named, and the rust stage comes first.** `COPY --from=rust` resolves against *stage names* first
  and an image reference second, so an unnamed `FROM rust:…` silently means `docker.io/library/rust:latest` — an extra
  unpinned pull and a toolchain nobody pinned, which the first version of the recipe did. And a stage cannot copy
  from itself, so the rust stage is the first `FROM` and node is the final image; `builder`'s own unit test reads the
  file for every `COPY --from=` name.
- **`CARGO_HOME` is not set.** Copied into a stage it is root-owned, and the build runs as the operator's uid, so
  cargo cannot write its registry (`failed to create directory /usr/local/cargo/registry/cache/…`, measured on the
  first real build after the stages were reordered). It uses `$HOME/.cargo` under `/build`, which the entry script
  creates and the container throws away; the same reason `HOME` is set there rather than defaulted.

The rust base's digest appears in **one** file, and `tests/pin_single_source.rs` — the test that keeps `pins/pi.txt`
single — walks every tracked file to prove it. The pin is the `FROM` line, which is where the two recipes that already
exist keep theirs; what a `pins/builder.txt` would add is a second place to read one value.

## D3 — the source is read-only, and the build happens in a copy

`docker run --rm --user <out's owner> -v <source>:/src:ro -v <out>:/out -e HOLOSMITH_GIT_TAG=<tag> <image> <triple>`.

- **`/src` is read-only.** A build cannot write to the tree it is built from — not `target/`, not `node_modules/`, not
  the repository. That is a second property beyond "it ran in a container", and it is free.
- **the entry script copies the tree into `/build`** (minus `target/`, `dist/`, `web/node_modules`, `web/dist` and
  `.git`) and runs `scripts/build.sh --target <triple> --no-check` there. One recipe for the artifact, run in here
  instead of on the host — not a second recipe that could drift from the one the gate proves.
- **`--no-check`** because `scripts/check-static.sh` boots the artifact inside a bubblewrap sandbox and fetches from
  loopback, a second namespace this container has no business needing. What it asserts about the bytes is read by the
  host instead (D4).
- **the source is the current directory.** An agent works in a worktree and runs the command there; a developer's
  checkout is where `git describe` finds the tag. There is no `--source` flag, and a directory that is not a Holosmith
  tree is refused before anything is mounted.
- **`--user` is the output directory's owner**, read from the filesystem because this build has no `getuid`: the
  artifact belongs to the operator, and the container does not run as root.

## D4 — the artifact is checked by the host, and placed where `upgrade` will read it

`--out DIR`, default `releases/<tag>` for the tag the mounted tree has — item 10's `releases/<tag>/` layout, whose
`current` symlink is that story's. Two files: `holosmith` and `holosmith.sha256` in `sha256sum -c`'s format.

The container writes `DIR/.holosmith.new`; the host then

1. **reads the ELF header of the bytes that landed**: `e_machine` for the triple that was asked for (`remote`'s
   62/183 reader), `ET_EXEC` and no `PT_INTERP` — the three properties `scripts/check-static.sh` asserts with `file`
   and `readelf`, read here from the bytes so the command refuses its own output before placing it. A container that
   exited 0 has told us it built *something*;
2. **hashes it with `images::sha256`** — the digest `server deploy`'s labels already use, so an operator's
   `sha256sum` and this value are the same algorithm;
3. **places it, or refuses**: a directory holding the *same* bytes is `Placement::Current` (nothing written, so a
   re-run is idempotent), and a directory holding **different** bytes is refused with both digests named and nothing
   replaced. Overwriting would leave one release directory holding two binaries for one tag — and `--dirty` gives a
   changed tree the same tag as the tree it was built from, so that is the ordinary case, not a hypothetical one.

## D5 — a failure prints the container's tail, and the exit code is 1

The container's output goes to a **file**, never a pipe, and a failed build prints the last
40 lines of it to stderr after one sentence naming the target and the image, then exits 1. Forty because a build's
last lines are the compiler saying what is wrong and `exit.rs`'s five are a container's dying words. A bound that
expires is its own sentence — an image build that could not reach the network, a build that did not finish.

`Docker::build_bounded` was generalised to `run_captured(arguments, input, bound)` for this: one primitive for "a
docker child whose output is a file and whose deadline is real", used by the two image builds and now by the container
run. Its signature and behaviour for the callers are unchanged.

## D6 — `scripts/verify.sh` keeps building on the host, and that is not a hole

The gate's `build` phase stays `scripts/build.sh`: it is fast, offline, and it is what `check-static.sh` and
`case_pwa` read (`dist/<triple>/holosmith`, the app byte for byte, the empty-root boot). Routing the gate through the
container would make the gate's own artifact depend on a daemon, a pull and six minutes of image build.

What keeps that from being a silence is `case_build`, and its shape is the decision:

- **the orchestration arm always runs**, with a `docker` stand-in on `PATH` that records every argv and writes a
  *real* static artifact into the `--out` mount, and `cargo`/`rustc` stand-ins that record any call and fail. It
  measures the recipe in the `--build-arg`, the two mounts, the tag the container is handed, the placement, the
  digest, the idempotent re-run, the refusal of a different binary, and the refusal of the wrong machine's artifact —
  and it **fails if the verb is deleted**, which is the rule a `SKIP` may not break (the gate rule).
- **the real arm runs when the machine can**: a daemon and the builder image already built. It asserts what only a
  real build can (`file` says statically linked, `readelf -d` finds no dynamic section, the digest is the file's).
  Without them it prints one note rather than a `SKIP`, because a `SKIP` here would fire exactly when the feature is
  deleted.

The two small fixes the story needed:

- **`scripts/build.sh` overwrote the tag its own comment says a builder supplies.** It now honours a value already set
  (`HOLOSMITH_GIT_TAG="${HOLOSMITH_GIT_TAG:-$(git describe …)}"`) and still falls back to `git describe`, then
  `unknown`. Measured: without the fix, the container's artifact reported `git_tag unknown`.
- **`build.rs`'s `HOLOSMITH_GIT_TAG` hatch already existed for this story** — `scripts/build.sh` was the only thing
  that could not use it.

## What this does not measure

No second architecture beyond the two the image carries (the cross toolchains are the ones the recipe pins, and the
aarch64 artifact was not built in the container in this story's demo), no offline machine (a cold image needs the
network for its bases, its toolchains and its registries), and the host's toolchains were *unavailable to the command*
in the demo, not uninstalled. Signing and provenance beyond a `.sha256` are out of scope, and `upgrade`/`rollback`
(those stories) and the release layout's `current` symlink are the next two stories.
