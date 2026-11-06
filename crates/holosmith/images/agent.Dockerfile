# holosmith: embedded agent image (HS-048)
#
# This is the image a session's container runs: pi at the pinned version, git, the toolchains the config names
# in `agent_toolchains`, and this binary for the helper subcommands (`container snapshot`, `proxy`).
#
# The base is node because pi is a node CLI — measured, `/usr/local/bin/pi` is a symlink into the npm global
# tree — and it is pinned by its **manifest-list digest**, so one Dockerfile resolves to the machine's own
# architecture.
#
# The image's own user is non-root (ADR-0035) and is the base's `node`. The invariant is that a session's files
# belong to the *invoking* user, and the gate asserts exactly that (`id -u` inside the image equals `id -u` on the
# host, and is not 0); uid 1000 is only what this repository's Linux machines and this base happen to spell it.
FROM node:24-bookworm-slim@sha256:01d566e8d94d8c619591d78652babfccd3c6992a3dffa9ed5ab39e11cf6c70e9

# The facts this binary knows about itself plus the toolchain list, passed in as build arguments and written as
# labels: `server deploy` compares them with its own values to decide whether an image is current, and `doctor`
# prints them. The binary's digest is among them because a git tag cannot tell two builds of the same commit (or
# of the same dirty tree) apart, and the deploy's skip rule is about the *binary*.
ARG HOLOSMITH_VERSION
ARG HOLOSMITH_GIT_TAG
ARG HOLOSMITH_PI_VERSION
ARG HOLOSMITH_BINARY_SHA256
ARG HOLOSMITH_TOOLCHAINS

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates openssl ${HOLOSMITH_TOOLCHAINS} \
 && rm -rf /var/lib/apt/lists/*

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
