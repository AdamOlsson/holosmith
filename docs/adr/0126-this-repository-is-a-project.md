# This repository is a project like any other

ADR-0123 gave a project a recipe of its own, and left the project that needed one most outside it: Holosmith's own
session image was `agent-image:<version>`, a third **product** image, built from a Dockerfile embedded in the binary and
stamped with `org.holosmith.*` labels. So the one tree everybody works in was the one tree whose toolchain lived in
`crates/holosmith/images/agent.Dockerfile` — a recipe that could not be a project's, because it was compiled into the
program that was supposed to be building projects. Every sentence in that file was true of this repository and false of
any other, which is exactly what a project's recipe is for.

The change is one move and one deletion:

- `docker/Dockerfile` at the repository root is this repository's own recipe, in the place `projectimage::RECIPE` reads
  a project's (`<project root>/docker/Dockerfile`, ADR-0123 D1). Its image is `hs-holosmith:<digest>`, so a session here
  resolves its image by the same function a session in any other project does.
- `crates/holosmith/images/agent.Dockerfile` is deleted with everything that existed to serve it: `images::AGENT_CONTEXT`,
  `images::Kind::Agent`, `Kind::context()`, `images::agent_image()`, the `AGENT_DOCKERFILE` const, build arguments built
  only for it, and the agent arm of step 3.

## D1 — the recipe is a project's, and the build arguments are declared by it

`docker/Dockerfile` is `FROM` the base image and copies nothing of the program, which is the shape every project's
recipe has. Two values its `FROM` line needs cannot come from a file, because a `FROM` cannot read one, so they arrive as
build arguments and `projectimage::Build::arguments` fills them from the values this binary already holds:

| argument | value | why it is not a literal |
| --- | --- | --- |
| `HOLOSMITH_BASE_IMAGE` | the base tag the project was resolved against | it is the same value the tag's digest covers (ADR-0123 D2), so a literal would be a second source for one fact |
| `HOLOSMITH_RUST_IMAGE` | `pins/rust.txt`'s whole reference | a digest in a recipe is the second copy ADR-0118 removed, and the pin must stay one edit |

**Both are declared by the recipe, and a build is handed only what it declares.** A `--build-arg` a recipe does not
declare is a BuildKit warning over nothing, so `declared_arguments` reads the recipe's own `ARG` lines and fills those:
a recipe with no `ARG` at all is built `FROM` a literal and handed nothing. The default
`ARG HOLOSMITH_BASE_IMAGE=holosmith-base:latest` in this repository's file exists only so a bare `docker build .` fails at
a resolvable `FROM` rather than at BuildKit's invalid-default warning; every build Holosmith runs passes the value.

The rust argument is the instruction **immediately before the first `FROM`**, which `tests/pin_single_source.rs`
asserts, and the stage it names is a *stage* rather than an image reference: `COPY --from=` resolves stage names first,
so a stage this file did not name would be `docker.io/library/rust:latest`.

**The context is the project root**, so the recipe's `COPY`s are this tree's real paths — the two manifests (the
workspace's and the member's, which share a basename and so need the scratch-tree trick), the member's `lib.rs`, and the
web pair. `.dockerignore` beside it keeps that context to the source tree: `target/`, `dist/`, `releases/`, `reports/`,
`toolchains/`, `.git`, `web/node_modules`, `web/dist` and `.pi` are not inputs of any build, and a context that carried
them would be gigabytes on every session's first start.

## D2 — the gate's container and a session's container are one artifact

`scripts/verify-in-container.sh` ran the whole gate in `agent-image:<version>` (ADR-0109), and `pass.rs` handed a
candidate's promotion the same image. Those are Holosmith building Holosmith too, so they stop naming an image of their
own and name **the candidate tree's project image** — `projectimage::resolve(&candidate.source, env!("CARGO_PKG_NAME"),
…)`. That is the first time the gate's container and a session's container are the same artifact on purpose: a gate that
passes in it is a gate that passed in what a session runs, and the toolchain a candidate is proven with is the toolchain
it will be worked on with.

Two consequences worth naming:

- **The wrapper builds the image when it is absent**, through `scripts/project-image.sh ensure`, because the gate cannot
  run before its image exists and the tag is a fresh digest on every recipe edit. It is refused rather than guessed when
  the wanted image is neither this tree's tag nor on the daemon.
- **The base the tag is composed over is the running release's** (`images::base_image()`), because that is the base
  image the machine has: a candidate that bumped `VERSION` carries no base of its own until it is promoted. `pass.rs`
  passes that value as `HOLOSMITH_BASE_IMAGE` and the wrapper composes the same tag from it, so a version-bump candidate
  builds its image `FROM` the base that exists instead of one nobody has built. A candidate tree *without* a recipe
  resolves to that base image, which is honest: a tree that cannot compile has no project image to run a gate in.

## D3 — `server deploy` builds two images, and the third is a project's to build

`images::Kind` has no `Agent` arm; `images::outstanding` iterates the proxy and the base; step 3's sentences, the hub's
`skip` line and the architecture guard's refusals all name two. A project's image is not a deploy's to build for the
reason ADR-0124 gives: the recipe arrives after the deploy does, in the project's own history, and a deploy that built
it would have to read a file it has no path to.

`images::Context` stays, because the archive the daemon is handed is still two entries plus a context — it is simply
empty for both product recipes now, and a project's recipe is built from a directory rather than from an archive.

`case_server_images` measures the third image where it can: it still owns the **absent** state, removes all three tags,
lets the deploy build the two product images, and then runs the `ensure` that builds this project's image — one real
`docker build` of `docker/Dockerfile`, which no Rust test can make. The `container snapshot --help` assertion that lived
in its agent arm dies with the arm: the binary is not in a session's image, and no product path calls that in-container
entry point.

## D4 — `python3` and `ripgrep` are the project recipe's

`scripts/verify.sh` asks `command -v python3` before every case that speaks a socket or reads a database by hand, and
turns each of those measurements into a `SKIP` when the program is missing. The gate now runs in the project image, so
the package that keeps those guards from firing belongs to the recipe the gate runs in — this repository's own. The same
is true of `file`, `build-essential`, `curl`, `git`, `openssl` and `procps`. Two measurements hold it, and they are the
static and the daemon half of one fact: `crates/holosmith/tests/images.rs` asserts `file`, `python3` and `ripgrep` in
`docker/Dockerfile`'s apt line, so an edit that dropped one fails in `cargo test`; and `case_server_images`' project arm
asks `dpkg-query -W` for `python3` and `ripgrep` **inside the image it just built**, which is what no test can answer —
a word that survived in the recipe whose package did not install. That arm is also where the image's own user is
measured, as the invoking uid and not root, which is ADR-0035's rule read off the artifact a session here runs. A
`SKIP` count that moved is still the defect this part of the decision exists to catch; what changed with the move is
that the count is now guarded by an assertion in the case that owns the image rather than by an arm of its own.

## D5 — what pins the smoke image's base

`smoke.Dockerfile` was held to the **agent** recipe's last `FROM` by a test, so the two could not drift onto different
node bases. That recipe is gone. The base recipe's own `FROM` is the pinned node digest now — this repository's project
recipe is `FROM ${HOLOSMITH_BASE_IMAGE}`, so it holds no node digest of its own — and the smoke recipe's test compares
the base recipe's `FROM` with the smoke recipe's, which is the same invariant with one fewer hop.

## The currency gap this inherits

The tag's digest covers the recipe's bytes and the base image's tag, and nothing else (ADR-0123 D2). `pins/rust.txt` is a
build argument, so it is **not** in the digest either: a bump of the pin changes what a project's image is built from
and leaves its tag where it was. `holosmith image build holosmith --force` is the answer, and it is the same class of
gap ADR-0123 D2 documents for a `COPY`'d file — a content address over the recipe cannot see a value the recipe is
handed. Closing it would mean putting the argument values into the digest, which is a second thing to keep in step
with `projectimage::tag`, so it is named here rather than half-done.

## What supersedes what

This decision supersedes the **agent-image parts** of ADR-0105, ADR-0106, ADR-0114 and ADR-0117, and two sentences of
ADR-0124 — D2's "this repository's `agent-image`", which named a product image this decision deletes, and D3's "slower
than any of the three product images", which are two now — and writes nothing over them: their claims about where a
toolchain is installed and why a session has no network are still the reasons, and only "the image is `agent-image`"
changes to "the image is this project's". ADR-0109's wrapper still runs the whole gate in a container with the daemon
outside it; the image it names is D2's. The rest of ADR-0123 and ADR-0124 is unchanged and now applies to this
repository as it does to any other project: a project's build is still lazier, is still the slow one, and still pays
for whatever its recipe installs.

## What is not measured

- **A real `docker build` of `docker/Dockerfile` in `cargo test`.** It is `case_server_images`, `case_agent_toolchain`
  and `case_agent_offline_build` — three daemon cases — and `crates/holosmith/tests/images.rs` holds only the recipe's
  shape and the two spellings of its tag.
- **A version-bump candidate's whole path.** D2 names the base the tag is composed over and the wrapper builds the tag,
  but a promotion whose candidate is a version ahead of the running release is not a case: it needs two releases on one
  machine.
- **A fresh `pins/rust.txt` bump.** The currency gap above: it needs `--force`, and nothing measures that an operator
  does.

## Consequences

- The tree has one fewer embedded recipe, one fewer `Kind`, and its own `docker/Dockerfile` — so the first project to
  bring a recipe is the project Holosmith is built in, which is the milestone's proof.
- A session in this repository needs no `base_image` key: the resolver finds the recipe and the container step builds
  `hs-holosmith:<digest>` on the first start that needs it (ADR-0124).
- `scripts/project-image.sh tag|ensure` is the one shell spelling of that tag, and `tests/images.rs` runs it against
  `projectimage::resolve` so the two cannot drift.
