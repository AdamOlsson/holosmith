# holosmith: embedded agent image
#
# This is the image a session's container runs: pi at the pinned version, git, the toolchains the config names
# in `agent_toolchains`, and this binary for the helper subcommands (`container snapshot`, `proxy`).
#
# It also carries the Rust toolchain — rustc, cargo, rustfmt, clippy, both musl targets and both musl.cc cross
# linkers — as part of the **fixed** set, not as an `agent_toolchains` entry: the tree a session works on is written
# in Rust, so `cargo fmt`, `cargo clippy` and `cargo test` are the same three commands in there as on a host. What
# it still does not carry is a Docker daemon, so the container-shaped integration binaries and `scripts/verify.sh`
# stay a host's work; a session measures the tree, not the machine.
#
# The base is node because pi is a node CLI — measured, `/usr/local/bin/pi` is a symlink into the npm global
# tree — and it is pinned by its **manifest-list digest**, so one Dockerfile resolves to the machine's own
# architecture.
#
# The image's own user is non-root (ADR-0035) and is the base's `node`. The invariant is that a session's files
# belong to the *invoking* user, and the gate asserts exactly that (`id -u` inside the image equals `id -u` on the
# host, and is not 0); uid 1000 is only what this repository's Linux machines and this base happen to spell it.
#
# **The rust stage comes first, and its name is load-bearing.** `COPY --from=rust` resolves against *stage names*
# first and an image reference second, so a stage this file did not name would be `docker.io/library/rust:latest` —
# an extra unpinned pull and a toolchain from whatever `latest` is that day. The reference is the same string
# `builder.Dockerfile`'s `FROM rust:` line carries, and a test asserts the two are identical, so a bump is two edits
# that cannot drift.
FROM rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS rust
FROM node:24-bookworm-slim@sha256:01d566e8d94d8c619591d78652babfccd3c6992a3dffa9ed5ab39e11cf6c70e9

# rustc, cargo and rustup, taken from the pinned rust stage rather than installed by a script: `curl … | sh` would
# put an unpinned installer in the build path, which is the thing a pinned stage exists to avoid. The stage is
# `rust:1.99.1-bookworm` rather than the image's node base because the two are not the same Debian — a toolchain
# copied across releases links against whatever libc its own base had.
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo

# The toolchain's paths, set **before** the `RUN`s that use them: this stage's base is node, which knows nothing
# about rustup, and an `ENV` at the end of the file is an environment no earlier instruction sees.
#
# **`RUSTUP_HOME` stays `/usr/local/rustup`.** Read-only is enough for `cargo` and `rustc`; only an install writes
# there, and every install in this file is a `RUN` that happens as root before `USER node`.
#
# **`CARGO_HOME` is `/tmp/cargo`, and that is the one difference from `builder.Dockerfile`, which deliberately sets
# nothing.** The builder runs as the operator's uid with a `$HOME` its entry script creates, so cargo's own default
# is writable. A session's container runs as the **data root's owner** — `--user <uid>`, not necessarily the image's
# `node`, and with no passwd entry at all, so `$HOME` is not a place anything can be put. `/tmp` is writable by any
# uid, so the registry is per-container and thrown away with it; a gate case mounts a warm `registry/` over this
# path instead, because a build with no network cannot fetch its crates.
#
# Never `RUSTFLAGS` (ADR-0005): it would disable `.cargo/config.toml`'s per-target `relocation-model=static` and
# silently ship a static-PIE binary — one container out, the same trap.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/tmp/cargo \
    PATH=/usr/local/cargo/bin:/opt/x86_64-linux-musl-cross/bin:/opt/aarch64-linux-musl-cross/bin:${PATH}

# The facts this binary knows about itself plus the toolchain list, passed in as build arguments and written as
# labels: `server deploy` compares them with its own values to decide whether an image is current, and `doctor`
# prints them. The binary's digest is among them because a git tag cannot tell two builds of the same commit (or
# of the same dirty tree) apart, and the deploy's skip rule is about the *binary*.
#
# `HOLOSMITH_TOOLCHAINS` is the *configurable* list and never names the Rust toolchain: rust is part of this recipe,
# so a machine cannot remove it and its version is the `FROM` line's rather than a config value's.
ARG HOLOSMITH_VERSION
ARG HOLOSMITH_GIT_TAG
ARG HOLOSMITH_PI_VERSION
ARG HOLOSMITH_BINARY_SHA256
ARG HOLOSMITH_TOOLCHAINS

# `build-essential` is the `cc` the `cc` crate needs for bundled SQLite — a session that runs `cargo test` compiles
# it from source — and `curl` is what fetches the two musl.cc tarballs below, at image-build time.
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential ca-certificates curl git openssl \
      ${HOLOSMITH_TOOLCHAINS} \
 && rm -rf /var/lib/apt/lists/*

# **Load-bearing**: `rust:1.99.1-bookworm` ships rustup's minimal profile, which is rustc, cargo and the standard
# library only. Without these two components `cargo fmt` and `cargo clippy` answer `'cargo-fmt' is not installed for
# the toolchain`, which is a session's first command failing on an image that looks complete.
RUN rustup component add rustfmt clippy

# Both musl targets, so the tree's own `.cargo/config.toml` targets resolve in a session: `rustup target add` fetches
# the standard library for them here, which is what keeps a later `cargo build --target …` offline apart from its
# crates.
RUN rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl

# The two cross toolchains, pinned by the digest of the tarball: musl.cc's downloads are not content-addressed, so the
# checksum is the pin. `x86_64-linux-musl-gcc` and `aarch64-linux-musl-gcc` are the linkers `.cargo/config.toml`
# names and the `cc` crate finds for the bundled SQLite, exactly as `~/.pi/toolchains/env.sh` provides them on a
# development machine — same URLs and same digests as `builder.Dockerfile`, because a session and a release build
# compile the same tree.
RUN curl -fsSL -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz \
 && echo "c5d410d9f82a4f24c549fe5d24f988f85b2679b452413a9f7e5f7b956f2fe7ea  /tmp/musl.tgz" | sha256sum -c - \
 && tar -C /opt -xzf /tmp/musl.tgz \
 && rm /tmp/musl.tgz
RUN curl -fsSL -o /tmp/musl.tgz https://musl.cc/aarch64-linux-musl-cross.tgz \
 && echo "c909817856d6ceda86aa510894fa3527eac7989f0ef6e87b5721c58737a06c38  /tmp/musl.tgz" | sha256sum -c - \
 && tar -C /opt -xzf /tmp/musl.tgz \
 && rm /tmp/musl.tgz

RUN npm install -g --omit=dev "@earendil-works/pi-coding-agent@${HOLOSMITH_PI_VERSION}" \
 && npm cache clean --force

COPY holosmith /usr/local/bin/holosmith

LABEL org.holosmith.version="${HOLOSMITH_VERSION}" \
      org.holosmith.git-tag="${HOLOSMITH_GIT_TAG}" \
      org.holosmith.pi-version="${HOLOSMITH_PI_VERSION}" \
      org.holosmith.binary-sha256="${HOLOSMITH_BINARY_SHA256}" \
      org.holosmith.toolchains="${HOLOSMITH_TOOLCHAINS}"

USER node
ENV PI_CODING_AGENT_DIR=/tmp/pi-agent
