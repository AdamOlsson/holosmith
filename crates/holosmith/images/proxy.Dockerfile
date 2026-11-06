# holosmith: embedded proxy image (HS-048)
#
# Both of the machine's proxies run this one image. `hs_egress` and `hs_key` are the same static binary with a
# different subcommand, so the image is the artifact on a minimal base and nothing else — no shell scripts, no
# language runtime, no second program that could be a way out.
#
# The base is pinned by its **manifest-list digest** rather than by its tag: one Dockerfile then resolves to
# whatever architecture the daemon building it is, which is what keeps a deploy on an arm64 machine from
# building an amd64 image it can never run.
#
# `openssl` is not optional. `hs_key` spawns `openssl s_client` for the TLS connection to the provider, because
# the artifact is static and carries no TLS stack of its own (ADR-0048), and `ca-certificates` is the trust
# half of that same sentence. `FROM scratch` is therefore not an option, however static the binary is: what the
# image is missing would be the client and the store the key proxy needs.
FROM alpine:3@sha256:3f4d6b32e26703c6082839234f43769d5046b3ac7c910033911d1e71d8cd630a

# The three facts this binary knows about itself, passed in as build arguments and written as labels: the
# image holds no build-time copy of them, so the labels are what a later deploy and `doctor` read back.
ARG HOLOSMITH_VERSION
ARG HOLOSMITH_GIT_TAG
ARG HOLOSMITH_PI_VERSION

RUN apk add --no-cache openssl ca-certificates

COPY holosmith /usr/local/bin/holosmith

LABEL org.holosmith.version="${HOLOSMITH_VERSION}" \
      org.holosmith.git-tag="${HOLOSMITH_GIT_TAG}" \
      org.holosmith.pi-version="${HOLOSMITH_PI_VERSION}"

ENTRYPOINT ["/usr/local/bin/holosmith"]
