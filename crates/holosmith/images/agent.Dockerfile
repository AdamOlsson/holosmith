# holosmith: embedded agent image
#
# This is this repository's own session image: pi at the pinned version, git, the Debian packages below that the gate
# needs, and this binary for the helper subcommands (`container snapshot`, `proxy`). A machine whose projects are this
# tree points `base_image` at it; the generic `base.Dockerfile` is the default, and a project that needs a package of
# its own builds an image `FROM` that one — a `docker/Dockerfile` of its own, `docker build -t <name> …` — and
# names it in `base_image`, rather than configuring a package list here.
#
# Everything this recipe installs is **fixed**. There is no package key and no toolchain label: a package a project
# needs goes in the image it builds for itself from its own `docker/Dockerfile`, which `base_image` then names, and
# the set here is what this repository's own gate and tree are measured to need. The Rust toolchain — rustc, cargo,
# rustfmt, clippy, the musl target and its musl.cc cross linker — is the largest part of it: the tree a session
# works on is written in Rust, so `cargo fmt`, `cargo clippy` and `cargo test` are the same three commands in there as
# on a host.
#
# **The Docker daemon stays outside, and the gate is not therefore a host's work alone.** A session's container carries
# no daemon, because a container with the socket can reach the host as root, and that is the isolation this product is:
# a session measures the tree, not the machine. What runs the whole gate in this image is
# `scripts/verify-in-container.sh`, on the operator's own daemon, with the socket, the `docker` CLI and the tree mounted
# for that run only (ADR-0109) — so this recipe is the one thing the host and the gate's container share, and the
# container-shaped integration binaries measure there instead of printing a `SKIP`.
#
# **That makes this image a machine this binary runs on, so it carries every program the binary spawns.** A gate in
# here runs `smoke`, whose liveness check and whose stop are `kill -0` and `kill -TERM` children (`src/smoke.rs`), and a
# Debian slim base has no `procps`: `kill` there is a shell builtin, and a builtin is not a program `Command::new` can
# find, so the child fails to spawn and a live candidate reads as `the candidate exited before printing its banner`.
# `procps` is in the apt line below for that reason. What is *not* added is every other name a command line might carry:
# the set is audited against each `Command::new` in `src/` rather than guessed, and what the gate supplies itself —
# `tailscale`, `systemctl`, `journalctl` and `loginctl` stand-ins, the mounted `docker` CLI — stays out of this recipe.
#
# **The crate set and the npm packages ship in the image too, as files.** A session's container has no network — the
# only thing it can reach is the egress proxy — so `cargo build` and `npm ci` have to be answered from the image and
# not from a registry: `cargo vendor --locked` writes this tree's `Cargo.lock` dependencies into `/opt/cargo-vendor`
# (105 crates, 108 MB) and `npm ci` fills `/opt/npm-cache` (12 MB) from `web/package-lock.json`. Both steps read the
# files the build context carries (`images::AGENT_CONTEXT`, six files at their real paths), which is why this
# recipe names files a bare `docker build .` of this repository also has. The point of them is a flag that is no
# longer needed: `cargo build --locked` and `npm ci` work bare, in a container with no network and an empty
# `CARGO_HOME`.
#
# **`/.cargo/config.toml` sits at the filesystem root, and that is what makes one file enough.** Cargo reads
# `.cargo/config.toml` from the working directory up through its parents, so a file at `/` is an ancestor of every
# `/work` a session gets, while the tree's own `.cargo/config.toml` — the musl linker and its
# `relocation-model=static` — is deeper and still wins on the keys it sets. It is written **after** the vendor step,
# and that order is load-bearing: a `directory` source pointing at a directory that does not exist yet fails with a
# message about the configuration rather than about the step that has not run.
#
# What a session still **cannot** do is add a crate. The vendored crate set is the lock's and nothing else,
# and there is no network to fetch a crate that is not in it. A new dependency is a lock edit and a rebuild of this
# image — a host's `cargo fetch`, not something a session can decide offline.
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
# an extra unpinned pull and a toolchain from whatever `latest` is that day.
#
# **The reference is `pins/rust.txt`'s and arrives as an argument, because a `FROM` line cannot include a file**
# (ADR-0118). The pin holds the **whole** reference — tag and digest — and `images::build_arguments` hands it to
# this recipe verbatim, so the value has one reader and one writer and a bump is one edit. It is declared
# **before the first `FROM`** because that is the only scope a `FROM` can read an argument from, and it costs no
# cache boundary: a stage's base is above every layer, and changing that reference was always a rebuild (ADR-0115).
ARG HOLOSMITH_RUST_IMAGE
FROM rust:${HOLOSMITH_RUST_IMAGE} AS rust
FROM node:24-bookworm-slim@sha256:01d566e8d94d8c619591d78652babfccd3c6992a3dffa9ed5ab39e11cf6c70e9

# rustc, cargo and rustup, taken from the pinned rust stage rather than installed by a script: `curl … | sh` would
# put an unpinned installer in the build path, which is the thing a pinned stage exists to avoid. The stage is the
# pinned rust base rather than the image's node base because the two are not the same Debian — a toolchain copied
# across releases links against whatever libc its own base had.
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo

# The toolchain's paths, set **before** the `RUN`s that use them: this stage's base is node, which knows nothing
# about rustup, and an `ENV` at the end of the file is an environment no earlier instruction sees.
#
# **`RUSTUP_HOME` stays `/usr/local/rustup`.** Read-only is enough for `cargo` and `rustc`; only an install writes
# there, and every install in this file is a `RUN` that happens as root before `USER node`.
#
# **`CARGO_HOME` is `/tmp/cargo`.** A session's container runs as the **data root's owner** — `--user <uid>`, not
# necessarily the image's `node`, and with no passwd entry at all, so `$HOME` is not a place anything can be put and
# cargo's own `$HOME/.cargo` default is unwritable. `/tmp` is writable by any uid, so whatever cargo writes there is
# per-container and thrown away with it. It starts **empty**: the crates a build needs are vendored into
# `/opt/cargo-vendor` below, and `/.cargo/config.toml` is what points cargo at them, so nothing has to be fetched and
# no mount has to be made.
#
# Never `RUSTFLAGS` (ADR-0005): it would disable `.cargo/config.toml`'s per-target `relocation-model=static` and
# silently ship a static-PIE binary — one container out, the same trap.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/tmp/cargo \
    PATH=/usr/local/cargo/bin:/opt/x86_64-linux-musl-cross/bin:${PATH}

# The one fact a `RUN` below spends, declared above it because an `ARG` is in scope of every instruction after it:
# `HOLOSMITH_PI_VERSION` is the pi version the install step pins. Every package this recipe installs is a literal in
# the line below, and a bump of the toolchain's own version is `pins/rust.txt`'s, which reaches the `FROM` above as
# an argument.
#
# **A build argument's scope is a cache boundary.** A `RUN` is in scope of every argument declared above it, and
# BuildKit puts that scope in the instruction's cache key whether or not the instruction names the value. So an
# argument that only the `LABEL` block at the bottom reads is declared there, below every `RUN`; a spent argument is
# declared above the step that spends it, and the layers between the two pay for a change to it as well — a pi bump
# rebuilds the apt line and every toolchain layer on the way to the install. A new git tag and a rebuilt binary are
# what every release hands this recipe, and neither may cost a rebuild of the toolchain or the pinned downloads
# (ADR-0115).
ARG HOLOSMITH_PI_VERSION

# `build-essential` is the `cc` the `cc` crate needs for bundled SQLite — a session that runs `cargo test` compiles
# it from source — `curl` is what fetches the pinned musl.cc tarball below, at image-build time, and `file` is what
# `scripts/check-static.sh` reads an artifact's link state with (`file -b`), so `scripts/build.sh` — whose last step
# that script is — cannot finish in a session without it. The `readelf` it also runs arrives with `build-essential`.
# `procps` is `/bin/kill`, the child the header says `smoke` needs and cannot get from a shell builtin.
#
# **`python3` and `ripgrep` are fixed here rather than configurable, and `python3` is the reason they cannot leave.**
# `scripts/verify-in-container.sh` runs this image as the gate's own machine, and `scripts/verify.sh` asks
# `command -v python3` before every case that speaks a socket or reads a database by hand — each of those guards turns
# its measurement into a `SKIP` when the program is missing, and a `SKIP` measures nothing. So the package is part of
# the recipe: an image without it is a gate that quietly runs fewer cases. `ripgrep` has no reader here at all and
# moves with it rather than being separated; a package of a *project's* own goes in the image that project builds for
# itself from its own `docker/Dockerfile`, which `base_image` then names.
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential ca-certificates curl file git openssl procps \
      python3 ripgrep \
 && rm -rf /var/lib/apt/lists/*

# **Load-bearing**: the pinned rust base ships rustup's minimal profile, which is rustc, cargo and the standard
# library only. Without these two components `cargo fmt` and `cargo clippy` answer `'cargo-fmt' is not installed for
# the toolchain`, which is a session's first command failing on an image that looks complete.
RUN rustup component add rustfmt clippy

# The musl target, so the tree's own `.cargo/config.toml` target resolves in a session: `rustup target add` fetches
# the standard library for it here, which is what keeps a later `cargo build --target …` offline apart from its crates.
RUN rustup target add x86_64-unknown-linux-musl

# The cross toolchain, pinned by the digest of the tarball: musl.cc's downloads are not content-addressed, so the
# checksum is the pin. `x86_64-linux-musl-gcc` is the linker `.cargo/config.toml` names and the `cc` crate finds for
# the bundled SQLite — the same URL and the same digest `scripts/build.sh` reads out of this recipe to fetch the same
# tarball on a host whose `toolchains/` is empty (ADR-0120), because a session and a release build compile the same
# tree.
#
# **The tarball is fetched here, at build time, and the artifact cannot carry it.** This recipe is
# `include_str!`ed into the binary and a deploy runs on a machine that has the artifact and nothing else, so the
# tarball can only be inside the artifact — 230 MB of someone else's binaries in a static binary — or fetched now
# (ADR-0111). The pin is what makes the fetch safe; the flags are what make it survive an outage.
# `--retry-all-errors` is not a second spelling of `--retry`: `--retry` retries a timeout and an HTTP 5xx and not the
# refused connection or the name that did not resolve, which is what musl.cc being away looks like from in here. A host
# that stays away is still a failed build, and the sentence `images::failure` writes for it says the fetch did not
# answer rather than that the recipe is wrong: `server deploy` exits 4 for it and the gate prints a line that measured
# nothing (ADR-0111).
#
# **The pin covers what arrives; the certificate covers only who served it.** The digest below is the integrity
# guarantee — the bytes that land in `/opt` are the bytes that were vetted, whoever answered — and transport
# authentication adds nothing the pin does not already hold. So a certificate that cannot be verified is a warning and
# a second attempt with `--insecure`, never a failed build: curl's own statuses 51, 60, 77 and 83 say the certificate is
# what failed, and **every other status stays fatal** — the refused connection, the name that did not resolve, the
# timeout, the HTTP 5xx and a digest that did not match are the failures they always were (ADR-0116). The line the
# fallback prints is `images::CERTIFICATE_WARNING`'s marker, which `images::ensure` replays to an operator rather than
# swallowing: a silent fallback is the one shape this repository does not do. `/bin/sh` is dash, so the fallback is a
# `case` on the child's status and nothing bash-shaped.
RUN if curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz \
      https://musl.cc/x86_64-linux-musl-cross.tgz; then :; else \
      status=$?; \
      case $status in 51|60|77|83) \
        echo "warning: musl.cc's certificate did not verify (curl exit $status): retrying that URL without certificate verification, and the pinned digest decides" >&2 ;; \
      *) exit $status ;; esac; \
      curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors --insecure -o /tmp/musl.tgz \
        https://musl.cc/x86_64-linux-musl-cross.tgz; \
    fi \
 && echo "c5d410d9f82a4f24c549fe5d24f988f85b2679b452413a9f7e5f7b956f2fe7ea  /tmp/musl.tgz" | sha256sum -c - \
 && tar -C /opt -xzf /tmp/musl.tgz \
 && rm /tmp/musl.tgz
RUN npm install -g --omit=dev "@earendil-works/pi-coding-agent@${HOLOSMITH_PI_VERSION}" \
 && npm cache clean --force

# The crate set and the web pair, in the context at their real paths. **The two `Cargo.toml` files share a
# basename**, and `COPY` keeps a source's name when the destination is a directory, so they cannot travel in one
# instruction: the workspace's becomes the scratch root's `Cargo.toml` and the member's goes to
# `crates/holosmith/Cargo.toml`, which is the layout `cargo vendor` resolves a workspace from. The `RUN` below reads
# the scratch root and removes it, so the image keeps the two directories that are the point and nothing else.
COPY Cargo.toml Cargo.lock /tmp/vendor-src/
COPY crates/holosmith/Cargo.toml /tmp/vendor-src/crates/holosmith/Cargo.toml
COPY crates/holosmith/src/lib.rs /tmp/vendor-src/crates/holosmith/src/lib.rs
COPY web/package.json web/package-lock.json /tmp/web/

# `cargo vendor --locked`: the lock's crates and only those, because a vendored crate set that is not the lock's is a
# build with other crates in it. **The scratch `CARGO_HOME` is this step's own and is removed in the same `RUN`**,
# so the layer keeps the 108 MB of unpacked crates and not a second copy of the same bytes as `.crate` archives.
# The session's `CARGO_HOME` stays `/tmp/cargo`, empty of crates: it is where cargo writes, not where it reads.
#
# The lock's own digest is written beside the crates. It is the one value that says whether a vendor directory is
# *this* tree's crate set, and `scripts/verify.sh`'s `case_agent_toolchain` compares it with the tree's `Cargo.lock`
# rather than trusting that two versions of a file agree. `a+rX` because a session runs as the data root's owner,
# which is neither root nor the image's `node` (ADR-0087), and reading these files is all it may do with them.
RUN cd /tmp/vendor-src \
 && CARGO_HOME=/tmp/cargo-vendor-home cargo vendor --locked /opt/cargo-vendor \
 && sha256sum Cargo.lock | cut -d' ' -f1 > /opt/cargo-vendor/.holosmith-cargo-lock \
 && rm -rf /tmp/vendor-src /tmp/cargo-vendor-home \
 && chmod -R a+rX /opt/cargo-vendor

# The replacement that directory needs, in the one file every `/work` sees (the header says why `/` and why after).
# `net.offline = true` is the second half: a build that asks for something the directory does not hold gets the
# `--offline` sentence, which names the missing dependency, instead of waiting on a network that is not there.
RUN mkdir -p /.cargo \
 && printf '%s\n' \
      '[source.crates-io]' \
      'replace-with = "vendored-sources"' \
      '' \
      '[source.vendored-sources]' \
      'directory = "/opt/cargo-vendor"' \
      '' \
      '[net]' \
      'offline = true' \
      > /.cargo/config.toml

# The npm half: `npm ci` against the web lock fills the cache with the tarballs a session's `npm ci` reads, and the
# `node_modules` it installs leaves with the scratch tree. `/opt/npm-cache` is named explicitly here because the
# `ENV` block below is not in effect yet — and must not be, since this step needs the network and that block says
# `offline`. The fill comes **after** the `npm cache clean --force` above for the reason it is a separate step at
# all: a clean of a cache that already held these packages is a cache with nothing in it.
RUN cd /tmp/web \
 && npm ci --cache /opt/npm-cache \
 && rm -rf /tmp/web \
 && chmod -R a+rX /opt/npm-cache

# The npm counterparts of `CARGO_HOME`, set after the steps they would have broken: `offline=true` is what makes a
# session's `npm ci` read `/opt/npm-cache` instead of asking a registry, and it is here rather than with the
# toolchain's `ENV` because the pi install and the cache fill above need the network. `npm_config_logs_dir` is the
# one path npm writes that is not the cache — its default is `<cache>/_logs`, and the cache is read-only for a
# session, which is a uid the image never created.
ENV npm_config_cache=/opt/npm-cache \
    npm_config_offline=true \
    npm_config_logs_dir=/tmp/npm-logs

COPY holosmith /usr/local/bin/holosmith

# The facts this binary knows about itself, passed in as build arguments and written as labels: `server deploy`
# compares them with its own values to decide whether an image is current, and `doctor` prints them. The binary's
# digest is among them because a git tag cannot tell two builds of the same commit (or of the same dirty tree) apart,
# and the deploy's skip rule is about the *binary*. There is no label for the package set: the set is a literal in
# this recipe, so the recipe's own digest — which this binary embeds — is what says it changed.
#
# Declared **here**, after the last `RUN`, for the reason stated with the argument above: a value nothing above
# this line reads may not be in the cache key of a step that does not read it (ADR-0115).
ARG HOLOSMITH_VERSION
ARG HOLOSMITH_GIT_TAG
ARG HOLOSMITH_BINARY_SHA256

LABEL org.holosmith.version="${HOLOSMITH_VERSION}" \
      org.holosmith.git-tag="${HOLOSMITH_GIT_TAG}" \
      org.holosmith.pi-version="${HOLOSMITH_PI_VERSION}" \
      org.holosmith.binary-sha256="${HOLOSMITH_BINARY_SHA256}"

USER node
ENV PI_CODING_AGENT_DIR=/tmp/pi-agent
