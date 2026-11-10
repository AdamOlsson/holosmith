# holosmith: the smoke image (HS-077, spec item 10 step 2)
#
# A session's smoke container runs **the candidate** as its agent: `/usr/local/bin/pi` is a two-line wrapper onto
# `holosmith container fake-pi`, so the launch, the ready handshake, the JSONL framing and the version check a
# promotion exercises are the shipped ones. `agent::version_of` asks this image's `pi` which release it is, and the
# wrapper answers the pinned version — the check stays measured instead of being skipped for the smoke run.
#
# The base is the agent image's own, by the same manifest-list digest, and **nothing is installed on it**: the fake
# replays records and needs no git, no CA bundle and no openssl. Node stays because the base has it and because the
# boot's network self-test runs its probe as `node -e` inside `agent_image` — which, in a smoke run, is this image.
# The two recipes naming one digest is what keeps them from drifting, and a test holds them together.
#
# The build context is `images::archive`'s: exactly `Dockerfile` and `holosmith`, so the wrapper is written by a RUN
# rather than copied as a third file.
FROM node:24-bookworm-slim@sha256:01d566e8d94d8c619591d78652babfccd3c6992a3dffa9ed5ab39e11cf6c70e9

# The candidate, byte for byte the file `holosmith smoke --binary` measured and labelled.
COPY holosmith /usr/local/bin/holosmith

# `pi` is the candidate. The wrapper passes pi's own arguments through — `--mode rpc`, `--no-session`, `--approve` and
# the row's `--model`/`--thinking` are the launch's, and `container fake-pi` accepts and ignores them — and `--version`
# is the one it answers. A symlink would not do: an argv[0] of `pi` reaches this binary's own parser, not its
# `container` subcommand.
RUN printf '#!/bin/sh\nexec /usr/local/bin/holosmith container fake-pi "$@"\n' > /usr/local/bin/pi \
 && chmod 0755 /usr/local/bin/pi

# The session's files belong to the invoking user (ADR-0035): the base's `node` is uid 1000, which is what this
# repository's machines and every Docker test already assume.
USER node
