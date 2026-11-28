# holosmith: this project's own session image
#
# A project may carry its own recipe at `<project root>/docker/Dockerfile`, and this is this repository's: the generic
# base image (`base_image`'s default, `holosmith-base:<version>`) with the toolchain a session needs to build *this*
# tree in it — rustc, cargo, rustfmt, clippy, the musl target and its musl.cc cross linker — plus the crate set and
# the npm packages shipped as files, because a session's container has no network. The build context is the project
# root, so the `COPY`s below are this tree's own paths.
#
# **Every package here is this project's, and that is the point of the file.** The base image is generic because
# another project's session must not inherit this repository's toolchain; this recipe is where a project says what it
# needs, in a file a reviewer reads in the same diff as the code that needs it. Nothing is a config key.
#
# **The base arrives as a build argument, and the rust stage beside it.** A `FROM` cannot read a file, so a recipe that
# wanted the base's tag or the rust base's digest from one would be a recipe holding a second copy of a value this
# binary already knows — which is the drift `pins/rust.txt` exists to prevent (ADR-0118, ADR-0126). The two arguments
# are declared in the one scope a `FROM` can read, above the first `FROM`, and `projectimage::Build` is what fills
# them: `HOLOSMITH_BASE_IMAGE` from the base tag the project was resolved against, and `HOLOSMITH_RUST_IMAGE` from the
# pin. The default below exists only so a bare `docker build .` fails at a resolvable `FROM` rather than at BuildKit's
# invalid-default warning; every build Holosmith runs passes the value.
#
# **`HOLOSMITH_RUST_IMAGE` is the instruction immediately before the first `FROM`.** Docker resolves an argument's
# global scope from the lines above the first `FROM`, and Rust is the stage whose base the argument is: it is a **stage**
# and not an image reference, because `COPY --from=` resolves against stage names first and an image reference second —
# a stage this file did not name would be `docker.io/library/rust:latest`, an extra unpinned pull of whatever `latest`
# is that day.
#
# **The crate set and the npm packages ship in the image too, as files.** A session's container has no network — the
# only thing it can reach is the egress proxy — so `cargo build` and `npm ci` have to be answered from the image and
# not from a registry: `cargo vendor --locked` writes this tree's `Cargo.lock` dependencies into `/opt/cargo-vendor`
# (105 crates, 108 MB) and `npm ci` fills `/opt/npm-cache` (12 MB) from `web/package-lock.json`. Both steps read the
# files the build context carries at their real paths, which is why the context is the project root and why
# `.dockerignore` beside this directory keeps that context to the source tree. The point of them is a flag that is no
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
# What a session still **cannot** do is add a crate. The vendored crate set is the lock's and nothing else, and there is
# no network to fetch a crate that is not in it. A new dependency is a lock edit and a rebuild of this image — a host's
# `cargo fetch`, not something a session can decide offline — and `scripts/check-crate-set.sh` is what says so in one
# sentence instead of leaving cargo to report a registry that is not there. **That script is this repository's, beside a
# recipe that is this project's**: it and the `sha256sum` clause below are the whole pattern a project that vendors
# content copies, and Holosmith reads neither (ADR-0127).
#
# **The binary is not here.** A session's image runs no `holosmith`: the three `--entrypoint` sites are the proxy
# containers, and the in-container `container snapshot` entry point has no caller in the product path. An image that
# carried one would be a copy of a program the container never runs.
#
# **The toolchain is what `scripts/build.sh` and `scripts/verify.sh` need, which is why `file` is in the apt line.**
# `scripts/check-static.sh` reads an artifact's link state with `file -b` and `scripts/build.sh` ends in it, so a
# session cannot finish a release build without it. `build-essential` is the `cc` the `cc` crate needs for bundled
# SQLite, and `readelf` arrives with it. `curl` is what fetches the pinned musl.cc tarball below, at image-build time.
#
# **`python3` and `ripgrep` are here because `scripts/verify.sh` asks for them.** `scripts/verify-in-container.sh` runs
# this image as the gate's own machine, and the gate asks `command -v python3` before every case that speaks a socket or
# reads a database by hand — each of those guards turns its measurement into a `SKIP` when the program is missing, and
# a `SKIP` measures nothing. So the package is part of this recipe, and a gate run in an image without it is a gate
# that quietly ran fewer cases. `ripgrep` has no reader here and moves with it rather than being separated.
#
# The image's own user is the base's `node` (ADR-0035). A session's container is started with `--user` over it, so this
# is the spelling a plain `docker run` of this image gets, and the invariant it belongs to is that a session's files
# belong to the *invoking* user.
ARG HOLOSMITH_BASE_IMAGE=holosmith-base:latest
ARG HOLOSMITH_RUST_IMAGE
FROM rust:${HOLOSMITH_RUST_IMAGE} AS rust
FROM ${HOLOSMITH_BASE_IMAGE}

# **`USER root` here, and it is not decoration.** The base image ends `USER node`, so a stage that starts `FROM` it
# begins as **uid 1000** — `node:24-bookworm-slim`, which the old single-file recipe built on, is root by default, and
# that is the difference this line carries over. Everything between here and the final `USER node` is a build step that
# writes where only root may: `apt-get` owns `/var/lib/apt/lists`, `rustup component add` and `rustup target add` write
# `/usr/local/rustup`, the musl.cc fetch unpacks into `/opt`, `cargo vendor` writes `/opt/cargo-vendor`, `mkdir -p
# /.cargo` writes the filesystem root, `npm ci --cache` fills `/opt/npm-cache`, and each `chmod -R a+rX` changes modes
# on files root owns. Measured: without this line the build dies at the apt line with `List directory
# /var/lib/apt/lists/partial is missing. - Acquire (13: Permission denied)`, and `tests/images.rs` asserts the line is
# here — nothing else about the recipe reads as wrong, which is what made it invisible until a real build ran.
USER root

# rustc, cargo and rustup, taken from the pinned rust stage rather than installed by a script: `curl … | sh` would put
# an unpinned installer in the build path, which is the thing a pinned stage exists to avoid. The stage is the pinned
# rust base rather than the image's node base because the two are not the same Debian — a toolchain copied across
# releases links against whatever libc its own base had.
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo

# The toolchain's paths, set **before** the `RUN`s that use them: this stage's base is the base image, which knows
# nothing about rustup, and an `ENV` at the end of the file is an environment no earlier instruction sees.
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

# The programs the header names, and each one a fact some code already relies on.
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
# **The tarball is fetched here, at build time, and the artifact cannot carry it.** A recipe of this tree's is built by
# the daemon and by `scripts/build.sh`, on a machine that reaches musl.cc; the pin is what makes the fetch safe and the
# flags are what make it survive an outage. `--retry-all-errors` is not a second spelling of `--retry`: `--retry`
# retries a timeout and an HTTP 5xx and not the refused connection or the name that did not resolve, which is what
# musl.cc being away looks like from in here. A host that stays away is still a failed build, and the sentence
# `images::failure` writes for it says the fetch did not answer rather than that the recipe is wrong: `server deploy`
# exits 4 for it and the gate prints a line that measured nothing (ADR-0111).
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

# The crate set and the web pair, in the context at their real paths. **The two `Cargo.toml` files share a basename**,
# and `COPY` keeps a source's name when the destination is a directory, so they cannot travel in one instruction: the
# workspace's becomes the scratch root's `Cargo.toml` and the member's goes to `crates/holosmith/Cargo.toml`, which is
# the layout `cargo vendor` resolves a workspace from. The `RUN` below reads the scratch root and removes it, so the
# image keeps the two directories that are the point and nothing else.
COPY Cargo.toml Cargo.lock /tmp/vendor-src/
COPY crates/holosmith/Cargo.toml /tmp/vendor-src/crates/holosmith/Cargo.toml
COPY crates/holosmith/src/lib.rs /tmp/vendor-src/crates/holosmith/src/lib.rs
COPY web/package.json web/package-lock.json /tmp/web/

# `cargo vendor --locked`: the lock's crates and only those, because a vendored crate set that is not the lock's is a
# build with other crates in it. **The scratch `CARGO_HOME` is this step's own and is removed in the same `RUN`**, so
# the layer keeps the 108 MB of unpacked crates and not a second copy of the same bytes as `.crate` archives. The
# session's `CARGO_HOME` stays `/tmp/cargo`, empty of crates: it is where cargo writes, not where it reads.
#
# The lock's own digest is written beside the crates, in the instruction that vendored them, so it is derived and cannot
# disagree with them: it is the one value that says whether a vendor directory is *this* tree's crate set, and
# `scripts/check-crate-set.sh` compares it with the tree's `Cargo.lock` rather than trusting that two versions of a
# file agree. `a+rX` because a session runs as the data root's owner, which is neither root nor the image's `node`
# (ADR-0087), and reading these files is all it may do with them.
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
# `node_modules` it installs leaves with the scratch tree. `/opt/npm-cache` is named explicitly here because the `ENV`
# block below is not in effect yet — and must not be, since this step needs the network and that block says `offline`.
RUN cd /tmp/web \
 && npm ci --cache /opt/npm-cache \
 && rm -rf /tmp/web \
 && chmod -R a+rX /opt/npm-cache

# The npm counterparts of `CARGO_HOME`, set after the step they would have broken: `offline=true` is what makes a
# session's `npm ci` read `/opt/npm-cache` instead of asking a registry, and it is here rather than with the
# toolchain's `ENV` because the cache fill above needs the network. `npm_config_logs_dir` is the one path npm writes
# that is not the cache — its default is `<cache>/_logs`, and the cache is read-only for a session, which is a uid the
# image never created.
ENV npm_config_cache=/opt/npm-cache \
    npm_config_offline=true \
    npm_config_logs_dir=/tmp/npm-logs

USER node
ENV PI_CODING_AGENT_DIR=/tmp/pi-agent
