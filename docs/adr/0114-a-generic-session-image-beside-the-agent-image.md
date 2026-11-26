# A generic session image beside the agent image

`agent.Dockerfile`'s header says it plainly: it carries rustc, cargo, rustfmt, clippy, both musl targets, both musl.cc
cross linkers, the vendored crate set, the npm cache and `/.cargo/config.toml` "as part of the **fixed** set, not as an
`agent_toolchains` entry: the tree a session works on is written in Rust". Nothing in that is wrong for Holosmith, and
all of it is wrong for the second project on the machine. Measured on this host: `agent-image:0.1.1` is **3.54 GB**, a
session working on a TypeScript or Go tree in it inherits a compiler it never uses and a vendor directory that is not
its own, and the image this ADR adds is **658 MB** built from the same base.

So this ADR adds a **second** session image and removes nothing. `holosmith-base:<version>` is the same session with pi,
node, git, CA certificates, `openssl` and `procps` in it and **nothing else**; `agent-image:<version>` stays exactly
what it is, because the session path still points at it and this repository still needs a container it can run its own
gate in. `server deploy`'s step 3 builds three images: proxy, base, agent.

**Superseded in part by ADR-0117:** `agent-image:<version>` is no longer what a session's container runs by default.
The key is `base_image`, defaulted to this decision's `holosmith-base:<version>`, and the agent image is what a
machine whose projects are this tree names explicitly.

**Supersedes ADR-0057's** sentence that "the two images are built from Dockerfiles embedded in the binary". That
decision's subject was *where the recipe lives* — inside the artifact, because a deploy runs on a machine that has the
artifact and nothing else — and it is unchanged: there are three `include_str!`ed recipes now, and
`scripts/check-static.sh` greps the artifact for all three markers. What is superseded is only the count, and the
sentence in ADR-0057 that a variant image is refused for the reason "two recipes for one role is the shape that
drifts": the base image is not a variant of `agent-image`'s role, it is the role without this repository's toolchain,
and ADR-0105's refusal was about a `agent.Dockerfile.rust` alternative to the *same* image.

## D1 — the six entries, and why not one of them is optional

A session image is a small contract, and every line of `base.Dockerfile` is a fact some code already relies on:

- **pi**, at the version in `pins/pi.txt`. `agent::version_of` runs `docker run --rm --network none --entrypoint pi
  <image> --version` and `version_check` requires that answer to equal the pin exactly, so a session image with
  another pi is a launch that refuses with a sentence about the two versions. The version arrives as
  `HOLOSMITH_PI_VERSION` — the same build argument the agent recipe takes — because a literal in the file would be a
  second copy of the pin, which `tests/pin_single_source.rs` refuses.
- **node**, because pi is a node CLI, and because the boot's isolation probe is `node -e <script>` run inside this
  image at every start (ADR-0044). That is why the base is node's rather than a bare Debian: the probe is a container
  started from the configured `agent_image`, and an image without node reports `node: not found` instead of a
  verdict about the machine.
- **git**, because a session commits: `<data>/sessions/<id>/pi-home/gitconfig` is written on every start and named by
  `GIT_CONFIG_GLOBAL`, which is what lets a plain `git commit` work in a container with no author to stamp
  (ADR-0103).
- **ca-certificates**, for the egress proxy's allow-listed HTTPS: the proxy terminates the session's CONNECT and opens
  its own TLS, but a session's own `git clone https://…` through that proxy is answered by a certificate chain the
  image has to trust.
- **openssl**, which a git-over-HTTPS path needs on this base, and **procps**, which is `/bin/kill`. The second one is
  not a preference: `smoke`'s liveness check and its stop spawn `kill -0` and `kill -TERM` as children
  (`src/smoke.rs`), a shell builtin is not a program `Command::new` can find, and a Debian slim base has no `procps` —
  measured once as a live candidate reading `the candidate exited before printing its banner`.

Nothing else is in it. No `build-essential`, no `file`, no rustup, no cross linker, no `/opt/cargo-vendor`, no
`/opt/npm-cache`, no `/.cargo/config.toml`.

## D2 — the absence is measured, not asserted

The difference from the agent image is the contract, so it is a gate case's assertion rather than a comment:
`case_server_images` runs, in the base image with `--network none` and as a uid that is neither 0 nor the image's own
user, `pi --version` (the pin), `git --version`, the tree's own `selftest-probe.js` through `node -e`, and
`sh -c 'command -v cargo'`, which must answer nothing. The same case asserts that `/opt/cargo-vendor`, `/opt/npm-cache`,
`/.cargo/config.toml` and `/usr/local/bin/holosmith` are absent with `--entrypoint test -e`.

**The uid is the identity rule, re-measured (ADR-0087).** A session's container runs `--user <the data root's
owner>:<gid>` — a uid with no passwd entry and `HOME` at `/` — so an image that needs `$HOME` fails here. That is why
`PI_CODING_AGENT_DIR` is an absolute path, and it is the only writable place the recipe names. The case reads the
image's own uid and picks one that is neither it nor zero, so the measurement cannot quietly become a plain
`docker run`.

## D3 — the base is the agent image's base, by the same manifest-list digest

`base.Dockerfile`'s `FROM` is `agent.Dockerfile`'s **last** `FROM`, character for character:
`node:24-bookworm-slim@sha256:01d566e8…`. Two reasons, and the first is the one that has to hold through this change:

- the smoke image's invariant — `the_smoke_recipe_names_the_agent_images_base` in `src/smoke.rs` — compares the smoke
  recipe's last `FROM` with the agent recipe's, so a base that drifted from the agent's would leave that test green
  while a session moved between two distributions;
- a session that moves from the base image to the agent image moves within one Debian release, so a program that runs
  in one runs in the other.

`tests/images.rs` compares the two lines and asserts the digest is present, so a bump is two edits that cannot drift —
the same rule ADR-0105 D2 applies to the rust stage and `builder.Dockerfile`, for the same reason: a Dockerfile cannot
`include` anything, so the copy is held together by a test rather than by a mechanism.

## D4 — four labels, no binary, and the digest that is still spent

The base image carries `org.holosmith.version`, `-git-tag`, `-pi-version` and `-binary-sha256`, and **not**
`org.holosmith.toolchains`: there is no toolchain list to carry, and a label for an empty value would be a key that
says nothing while costing a rebuild rule. The three labels are one arm in `Kind::tags`, so "which labels does this
image have" stays one answer that `Expectation::mismatches`, `server deploy`'s skip rule and `doctor` all read.

**The binary is not copied in, and that is proven rather than assumed.** `images::BINARY_IN_IMAGE` is reached by three
`--entrypoint` sites and all three are the proxy containers; the one in-container entry point left, `holosmith
container snapshot`, has no caller in the product path, because the Docker backend runs `snapshot::take` itself in
`spawn_blocking` (`backend/docker.rs`). So the recipe has no `COPY` at all.

The digest label is still there, and it is the **build's** fact rather than the image's contents: these bytes went into
this recipe. Without it a rebuilt artifact would leave every label equal and the skip rule would answer `satisfied`
over an image built from the previous program — the hole ADR-0057's fifth label closed, and it does not reopen because
one of the three recipes copies nothing. The archive still carries the binary's bytes for every kind (one archive
shape, and `images::archive` reads the file before the daemon is asked anything); the base image is simply the one
recipe that never `COPY`s them.

`Kind::Base.context()` is the zero-length slice: the recipe installs no Rust, so there is no crate set for a build
context to reach — the proxy image's arm, and for the same reason.

## D5 — the order is proxy, base, agent

Step 3 builds in the order the machine's cost is in: the proxy image is what step 4 starts, so a machine that runs out
of bounds or disk has the image its proxies need first; the base image is a node base plus five apt packages and one
`npm install -g`, which is minutes; the agent image is the same base plus a 2.2 GB rust stage, two musl.cc tarballs, a
vendored crate set and an npm cache, which is the ten minutes ADR-0105 and ADR-0106 measured. `images::outstanding`
walks the same three in the same order, so the plan's reasons and the build's notes agree.

The promotion path is unchanged in shape and wider in scope: `upgrade`/`rollback` read and rebuild what the labels say
is stale, which is now three images, and `tests/upgrade.rs`'s trace names all three reads before all three builds.

## D6 — the skip rule reads the base image the way it reads the agent image's

`Expectation::of_release(Kind::Base, …)` has `toolchains: None` and the four shared keys, and the base image's labels
are written from exactly those build arguments, so a second deploy is honestly `satisfied` for all three. That is
asserted two ways with no daemon: `tests/deploy.rs` carries a fake `docker` whose base image is labelled from
`images::build_arguments`, and the second run must say `satisfied` and build nothing; `tests/images.rs` drives
`outstanding` over a fake whose base labels are one build's old bytes.

`tests/upgrade.rs`, `tests/update.rs`, `function fake_product_images` in `scripts/verify.sh` and `case_server_images`
all name the tags they fake or build, and each gained `holosmith-base:<version>`: a stand-in whose `case` patterns do
not know a tag falls through to the real daemon, which would have made a promotion's third build a real one on a
machine whose gate is faking the other two.

## D7 — what this does not decide

- **`agent_image` still defaults to `agent-image:<version>`.** A machine whose projects are not Rust points that key at
  `holosmith-base:<version>` by hand; nothing in the config chooses an image by a project's language, and no session is
  started in the base image unless an operator says so. The key's default is the image this repository's own gate runs
  in, which is the one thing a deploy can assume.
- **Two images are built on every machine, and the base one is not free.** A deploy that has never run pays for both.
  The alternative — building only the configured image — would make a machine's step 3 sentence about a tag it did not
  install, and the skip rule would have nothing to compare for the one an operator switches to later. This is the same
  trade ADR-0057 made for the proxy image, which a `--backend stub` machine also never runs.
- **No shared parent recipe.** The base image is not `FROM agent-image`, and the agent recipe is not edited to build
  `FROM holosmith-base:<version>`: a second tag would make the agent image's identity depend on a tag whose labels the
  daemon does not compare, and the two recipes share their base *reference* — held by a test — rather than a built
  image. A `FROM` on a locally built tag is also a build that cannot run before the image it names exists.

## What is not measured here

- **A deploy's step-3 line and the second run's `satisfied` over a real daemon**: `case_server_images` measures both,
  and it is the parent's `scripts/verify.sh` that runs it. What a worker's gate can measure is the throwaway build of
  this recipe and the four assertions of D2 by hand.
- **A session actually working in the base image.** No session path creates one, by D7: the case measures the image's
  contents, not a turn.
- **The build's cost.** The base image's own `docker build` was run by hand while this ADR was written — one apt
  layer and one `npm install -g`, about a minute with the node base already pulled, and 658 MB on disk — but no case
  bounds it, and `BUILD_TIMEOUT` stays the agent image's forty minutes because a build that cannot finish stops the
  step with a sentence rather than a number that has to be met.

## Consequences

- `server deploy`'s step 3 builds three images and its `changed` and `satisfied` sentences name all three; the
  progress line says "building the three images".
- `holosmith-base:<version>` is on every machine a deploy has run on, and `test -z "$(docker run --rm --entrypoint sh
  holosmith-base:<version> -c 'command -v cargo')"` is the measurement that says it is generic.
- `images::Kind` has three arms, `images::base_image()` is the one spelling of the tag, and
  `scripts/check-static.sh` greps the artifact for three recipe markers.
- The documents that describe *the* session image are unchanged: a separate change owns the words for "the image a
  session runs" now that there are two of them.
