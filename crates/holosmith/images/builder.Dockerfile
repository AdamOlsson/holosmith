# holosmith: the pinned builder container (HS-076)
#
# This is the image `holosmith build` runs, and the only place a release artifact is compiled. Spec item 10's first
# step is that the build never runs on the host, so that a `build.rs`, a proc macro — or the web app's own `vite`
# config, which is JavaScript the build executes — cannot run outside a container.
#
# The image is **ours** rather than a published `rust:` tag, because it needs three things no single official image
# has: rustc with both musl targets, the two musl.cc cross toolchains, and node 24 for the web app. Both of its
# bases are pinned by **manifest-list digest**, so one recipe resolves to the machine's own architecture (the same
# rule as `proxy.Dockerfile` and `agent.Dockerfile`), and the musl.cc tarballs are pinned by their SHA-256 in the
# `RUN` lines below — a download is a pin only if its bytes are checked.
#
# It carries no `holosmith` binary: the product images are built from this program, and this image builds the
# program. Its `build-entry.sh` is the whole of what it runs.
FROM node:24-bookworm-slim@sha256:01d566e8d94d8c619591d78652babfccd3c6992a3dffa9ed5ab39e11cf6c70e9 AS node
FROM rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c

# rustc, cargo and rustup, taken from the pinned rust image rather than installed by a script: `curl … | sh` would
# put an unpinned installer in the build path, which is the thing a pinned builder exists to avoid.
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo

# node and npm, taken from the pinned node image for the web app (`scripts/build-web.sh` runs `npm ci`, the
# transcript's tests and Vite). Copying the two files and recreating npm's two symlinks is the whole of it; the
# node image and this base are the same Debian release, so the binary's libraries are here.
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx

# `build-essential` is the `cc` the `cc` crate needs for bundled SQLite on the host's own architecture, and git is
# what `scripts/build.sh` asks for the tag (it answers `unknown` in a copy without `.git`, and `holosmith build`
# passes `HOLOSMITH_GIT_TAG` instead).
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential ca-certificates curl git \
 && rm -rf /var/lib/apt/lists/*

# Both musl targets, so one image builds either artifact. `rustup target add` fetches the standard library for the
# targets at image-build time, which is what keeps a *release* build offline apart from its crates.
RUN rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl

# The two cross toolchains, pinned by the digest of the tarball: musl.cc's tarballs are not content-addressed, so
# the checksum is the pin. `x86_64-linux-musl-gcc` and `aarch64-linux-musl-gcc` are what `.cargo/config.toml` names
# as the linkers and what the `cc` crate finds for the bundled SQLite, exactly as `~/.pi/toolchains/env.sh` provides
# them on a development machine.
RUN curl -fsSL -o /tmp/musl.tgz https://musl.cc/x86_64-linux-musl-cross.tgz \
 && echo "c5d410d9f82a4f24c549fe5d24f988f85b2679b452413a9f7e5f7b956f2fe7ea  /tmp/musl.tgz" | sha256sum -c - \
 && tar -C /opt -xzf /tmp/musl.tgz \
 && rm /tmp/musl.tgz
RUN curl -fsSL -o /tmp/musl.tgz https://musl.cc/aarch64-linux-musl-cross.tgz \
 && echo "c909817856d6ceda86aa510894fa3527eac7989f0ef6e87b5721c58737a06c38  /tmp/musl.tgz" | sha256sum -c - \
 && tar -C /opt -xzf /tmp/musl.tgz \
 && rm /tmp/musl.tgz

# The build directory is the container's own writable space: the source is mounted read-only at `/src`, so a build
# can neither reach nor modify the operator's tree, and everything a build writes (the copy of the source, `target/`,
# `node_modules/`, cargo's registry cache) lands here and goes away with the container.
RUN mkdir -m 0777 /build

COPY build-entry.sh /usr/local/bin/holosmith-build

# The recipe's own digest, passed in as a build argument and written as a label: `holosmith build` reads it back to
# decide whether the image it finds was built from *this* Dockerfile and entry script. A version tag cannot answer
# that — editing the recipe without bumping the version would otherwise reuse a stale image, which is the same class
# of mistake `org.holosmith.binary-sha256` exists to prevent on the product images (HS-048).
ARG HOLOSMITH_BUILDER_RECIPE
LABEL org.holosmith.builder-recipe-sha256="${HOLOSMITH_BUILDER_RECIPE}"

# Never `RUSTFLAGS` (ADR-0005): it would disable `.cargo/config.toml`'s per-target `relocation-model=static` and
# silently ship a static-PIE binary — one container out, the same trap.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/opt/x86_64-linux-musl-cross/bin:/opt/aarch64-linux-musl-cross/bin:/usr/local/bin:/usr/bin:/bin

ENTRYPOINT ["/bin/sh", "/usr/local/bin/holosmith-build"]
