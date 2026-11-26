# holosmith: embedded base image
#
# The **generic** session image: pi at the pinned version, node under it, git, CA certificates, `openssl` and `procps`
# — and nothing else. It is what a project that is not this repository runs a session in; `agent.Dockerfile` is the
# same session with this tree's toolchain, crate set and npm cache in it.
#
# Every entry below is a fact some code already relies on:
#
# - **pi**, because `agent::version_of` runs `docker run --rm --network none --entrypoint pi <image> --version` and
#   `version_check` requires that answer to equal the pin exactly. The version arrives as `HOLOSMITH_PI_VERSION`
#   rather than as a literal, so `pins/pi.txt` stays the one copy of it.
# - **node**, because pi is a node CLI, and because the boot's isolation probe is `node -e <script>` run inside this
#   image at every start (ADR-0044). That is why the base is node's rather than a bare Debian.
# - **git**, because a session commits: `<data>/sessions/<id>/pi-home/gitconfig` is written on every start and named by
#   `GIT_CONFIG_GLOBAL`, which is what lets a plain `git commit` work in a container with no author to stamp
#   (ADR-0103).
# - **ca-certificates**, for the egress proxy's allow-listed HTTPS.
# - **openssl**, which a git-over-HTTPS path needs, and **procps**, which is `/bin/kill`: `smoke`'s liveness check and
#   its stop spawn `kill -0` and `kill -TERM` as children (`src/smoke.rs`), and a shell builtin is not a program
#   `Command::new` can find. That sentence is why `procps` is in the agent image's apt line too.
#
# **What is absent is as deliberate as what is present**, and it is measured rather than asserted: no
# `build-essential`, no `file`, no rustc or cargo, no cross linker, no `/opt/cargo-vendor`, no `/opt/npm-cache` and no
# `/.cargo/config.toml`. A tree that needs any of them runs in the agent image, which carries all of it. This image is
# generic, and a second project's session must not inherit this repository's toolchain because this repository
# happens to be written in Rust.
#
# **The binary is not copied in.** `images::BINARY_IN_IMAGE` is reached by three `--entrypoint` sites and all three are
# the proxy containers, while `holosmith container snapshot`'s in-container entry point has no caller in the product
# path — the Docker backend runs `snapshot::take` itself in `spawn_blocking` (`backend/docker.rs`). So nothing here
# carries a holosmith, and the `binary-sha256` label below is the digest of the bytes a build *would* have copied: it
# is what `server deploy`'s skip rule compares to see a rebuilt artifact, and it moves with the recipe because the
# recipe is `include_str!`ed into that binary.
#
# **The base is `agent.Dockerfile`'s own base, by the same manifest-list digest**, so a session that moves between the
# two images does not move to another distribution and the smoke image's `the_smoke_recipe_names_the_agent_images_base`
# stays true through the second recipe. `tests/images.rs` reads this file's `FROM` line against the agent recipe's
# last one, so the two cannot drift. The **digest** rather than the tag is the pin: a tag re-resolves on the next
# pull, and one manifest list resolves to the daemon's own architecture.
#
# The identity rule is unchanged (ADR-0087): a session's container runs `--user <the data root's owner>:<gid>` with no
# passwd entry and `HOME` at `/`, so nothing here may need `$HOME`. `PI_CODING_AGENT_DIR` is an absolute path for
# that reason, and it is the only writable place this recipe names.
FROM node:24-bookworm-slim@sha256:01d566e8d94d8c619591d78652babfccd3c6992a3dffa9ed5ab39e11cf6c70e9

# The one fact a step below spends, declared above it because an `ARG` is in scope of every instruction after it:
# `HOLOSMITH_PI_VERSION` is the pi version the install step pins, and it is the only argument here whose value changes
# what this image holds.
#
# **A build argument's scope is a cache boundary.** A `RUN` is in scope of every argument declared above it, and
# BuildKit puts that scope in the instruction's cache key whether or not the instruction names the value. So an
# argument that only the `LABEL` block at the bottom reads is declared there, below every `RUN`; a spent argument is
# declared above the step that spends it, and the layers between the two pay for a change to it as well — a pi bump
# rebuilds the apt line between here and the install. A new git tag and a rebuilt binary are what every release hands
# this recipe, and neither may cost those two layers (ADR-0115).
ARG HOLOSMITH_PI_VERSION

# The five programs the header names and the trust store they need. `--no-install-recommends` because a
# recommendation is a package the contract did not ask for, and the point of this recipe is that its contents are
# enumerable.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git openssl procps \
 && rm -rf /var/lib/apt/lists/*

# pi from npm at the pinned version, and then the npm cache cleaned away: it is 12 MB of tarballs this image has no
# `npm ci` to answer, so it is not part of what a session here can rely on. The install is as root, before `USER node`,
# because a global package lives in `/usr/local/lib/node_modules` — `/usr/local/bin/pi` is the symlink into it that
# `agent::version_of` runs.
RUN npm install -g --omit=dev "@earendil-works/pi-coding-agent@${HOLOSMITH_PI_VERSION}" \
 && npm cache clean --force

# The facts this binary knows about itself, passed in as build arguments and written as labels. `server deploy`'s
# step 3 compares them with its own values to decide whether an image is current, and `doctor` prints the agent
# image's as the one a session runs. There is no `HOLOSMITH_TOOLCHAINS`: this recipe installs no toolchain, so there
# is no list to carry and no `org.holosmith.toolchains` label — a key for an empty value would be a label that says
# nothing while costing a rebuild rule.
#
# Declared **here**, after the last `RUN`, for the reason stated with the argument above: a value nothing above this
# line reads may not be in the cache key of a step that does not read it (ADR-0115).
ARG HOLOSMITH_VERSION
ARG HOLOSMITH_GIT_TAG
ARG HOLOSMITH_BINARY_SHA256

LABEL org.holosmith.version="${HOLOSMITH_VERSION}" \
      org.holosmith.git-tag="${HOLOSMITH_GIT_TAG}" \
      org.holosmith.pi-version="${HOLOSMITH_PI_VERSION}" \
      org.holosmith.binary-sha256="${HOLOSMITH_BINARY_SHA256}"

# The base's own non-root user (ADR-0035), the same one the agent image keeps for the same reason. A session's
# container is started with `--user` over it, so this is the spelling a plain `docker run` of this image gets rather
# than the identity a session has; `case_server_images` measures the second one as a uid that is neither 0 nor this.
USER node
# The one path pi writes that must not be `$HOME`: a session's `HOME` is `/` and belongs to no writable place.
ENV PI_CODING_AGENT_DIR=/tmp/pi-agent
