# The two images are built from Dockerfiles embedded in the binary

This is spec §"What `server deploy` does" step 3 and the **Build and release** section's third bullet, and
its acceptance says five things: both Dockerfiles are embedded and `holosmith-proxy:<version>` is the static
binary on a minimal base that serves both proxies; `agent-image:<version>` pins the pi version and its base
image digest and carries git, the configured toolchains and the binary for the helper subcommands; builds are
skipped when the binary and the pinned versions are unchanged; the image labels carry version, git tag and
pinned pi so `doctor` can compare them; and a build failure names the layer and leaves no half-tagged image.
The demo is the same claim from the other side: a fresh host ends with `holosmith-proxy:<version>` and
`agent-image:<version>`, both pinned.

The reason a decision is needed at all is that a Dockerfile is a file and a deploy runs on a machine that has
the artifact and nothing else. "So that there is no separate artifact to keep in sync" is a statement about
*where the recipe lives*, and every decision below follows from putting it in the binary, together with what
the two images must contain to be the images the rest of this build already assumes.

## The Dockerfile is embedded, and the context is a tar on stdin

`include_str!` twice — `images::PROXY_DOCKERFILE` and `images::AGENT_DOCKERFILE` — so the recipe is bytes in
the artifact, and `build.rs` re-runs when `images/` changes because a reworded Dockerfile is a different
binary.

A build needs a **context**, and the payload of that context is the running binary. There are two honest
ways to hand it over: a context directory (`target/…`) or an archive on `docker build -`'s stdin. The archive
is the stronger one and it is what this build does. A directory is a path, and a path handed to the daemon is
resolved in the **host's** namespace, which is the landmine that has cost this project a cycle at,
 and: a source the daemon cannot see becomes an empty root-owned directory, silently, and the
build then fails on a `COPY` inside the image rather than at the boundary where the mistake was made. A tar on
a pipe has no path at all. Measured on Docker 29.9.2: `docker build -t x -` from a 6.5 MB archive holding
`Dockerfile` and `holosmith` builds, and the archive is reproducible because its `uid`, `gid` and `mtime`
fields are zero (`images::archive` writes ustar by hand rather than taking a dependency for two entries).

**The build's output cannot go to a pipe.** A build prints hundreds of lines; a pipe nobody drains fills at
64 KiB and the build deadlocks. So `Docker::build_bounded` sends stdout and stderr to a temporary file, writes
the archive from a **spawned thread** (a deadline is worthless if the call is stuck in `write`), polls
`try_wait` to the bound, kills the child on expiry, and reads the file back. `Ok(None)` means "did not
finish", which is a sentence, not a hang: `BUILD_TIMEOUT` is twenty minutes, because a first build installs pi
and the toolchains over the network.

## The proxy image is `alpine:3` **with openssl**, and that is not a preference

`FROM scratch` is the obvious answer for a static musl binary and it is **wrong here**: `hs_key` spawns
`openssl s_client` for the TLS connection to the provider (ADR-0048), so an image without openssl answers 502
to every model call, and `ca-certificates` is the trust half of the same sentence. So the base is
`alpine:3@sha256:…` plus `apk add --no-cache openssl ca-certificates`, and the only other content is
`COPY holosmith /usr/local/bin/holosmith` — no shell scripts, no runtime, no second program that could be a
way out. Measured: both `proxy egress` and `proxy key` run from it with no mount, `openssl version` answers
3.5.9, and `version` prints the binary's own lines.

The agent image is `node:24-bookworm-slim@sha256:…`, because **pi is a node CLI**: measured,
`/usr/local/bin/pi` is a symlink into the npm global tree, and an image without node is exit 127 at launch,
not a test failure. It installs `git ca-certificates openssl` and the configured
toolchains, `npm install -g "@earendil-works/pi-coding-agent@<pin>"`, copies the binary in for the helper
subcommands (`container snapshot`, `proxy`), and keeps the base's `node` user — uid 1000, the host user's —
because a session that wrote its worktree as root leaves files nobody on the host can commit to (ADR-0035).
`PI_CODING_AGENT_DIR=/tmp/pi-agent` is the shape `pi-agent:local` is measured with; a session's launch
overrides it.

## Both bases are pinned by a **manifest-list** digest

A tag floats; the acceptance says the base is pinned. The digest recorded is the **index** digest — the one
that resolves per architecture — rather than a per-platform one, because one Dockerfile then serves both musl
targets: an arm64 daemon resolves the same `FROM` line to its own image. Measured: `alpine:3` is
`sha256:3f4d6b32…` and `node:24-bookworm-slim` is `sha256:01d566e8…`, and `docker manifest inspect -v` does
*not* list those digests (they are the lists, not their members), which is the trap in checking the pin by
hand. Refreshing a digest is a deliberate edit to the Dockerfile.

## The pins arrive as build arguments, so `pins/pi.txt` stays the only copy

The Dockerfiles contain no version literal. `HOLOSMITH_VERSION`, `HOLOSMITH_GIT_TAG`, `HOLOSMITH_PI_VERSION`
and `HOLOSMITH_BINARY_SHA256` come from the binary's own `env!`, `pins::pi_version()` and a digest of its own
bytes as `--build-arg`s, and the Dockerfile writes them as labels. The pinned pi version therefore has exactly
one tracked copy, which `tests/pin_single_source.rs` already guards and which now also covers the two new
files — the test's own `include_str!` of `pins/pi.txt` is what `tests/images.rs` compares them against.

## The labels are the read: five, and two of them exist because a tag cannot answer

`org.holosmith.version`, `org.holosmith.git-tag`, `org.holosmith.pi-version` and
`org.holosmith.binary-sha256` on both images, plus `org.holosmith.toolchains` on the agent image.

Two are not decoration, and each closes a hole the other labels leave open:

- **`toolchains`** — without it, a changed `agent_toolchains` would leave every label equal and the skip rule
  would answer `satisfied` for an image whose contents had changed. It is space-joined as `--build-arg` (the
  Dockerfile's `apt-get install … ${HOLOSMITH_TOOLCHAINS}` reads it as several packages) and written as the
  same string, because a `LABEL` line cannot rewrite what it was given.
- **`binary-sha256`** — the acceptance's word is *the binary*, and the git tag does not name a binary. `git
  describe --tags --always --dirty` is identical for two builds of the same commit, and also for two builds of
  the same **dirty tree**, which is what every development machine has: edit a file, rebuild, deploy, and a
  label set of version + tag + pi says `satisfied` over an image holding the previous artifact while
  `hs_egress` keeps serving it. The digest is a read of the exact bytes the build's archive hands the daemon.

The digest is the strong form of "nothing else was needed": a build **timestamp** says nothing about what is
inside, and an image **id** changes on every rebuild of identical inputs, so both would turn a no-op deploy
into a rebuild. SHA-256 is written out in `images.rs` (fifteen lines of schedule and compression, checked
against the standard's own vectors) rather than taken from a crate, because the only question asked of it is
"are these the same bytes" and `--locked` means a dependency is a decision; an operator can compare the label
with `sha256sum` on the artifact, which is the point of choosing a standard digest over an opaque hash.

The labels are one word per fact and they are the image's: `pins/pi.txt` remains pi's single source, and the
label is what a *built* image says about itself.

## The names are the binary's, and the config's default is the same function

`holosmith-proxy:<CARGO_PKG_VERSION>` and `agent-image:<CARGO_PKG_VERSION>` are `images::proxy_image()` and
`images::agent_image()`, and `Config`'s default `agent_image` calls the second one, so the name has one
spelling. A config that names another image — this machine's gate sets `pi-agent:local` — still gets both
spec-named images built, and step 3's line says so (`; the config's agent_image names pi-agent:local, which
this step does not build`). A deploy must never re-tag the image an operator chose to hand-build.

## "The configured toolchains" is a config key, and that moves the golden file

The spec says "the toolchains named in the config", and there was no such key. A fixed set in the Dockerfile
under a sentence that says "configured" is the one thing this story may not write, so `agent_toolchains` is a
key: a list of Debian package names, default `["python3", "ripgrep"]`. Its consequences are the documented
ones — `config/schema.rs`'s `KNOWN_KEYS.len()` is 33 and `tests/golden/config-default.toml` moves in the same
commit — and one of its own: an **empty list is legitimate** (a machine whose projects need only git), so only
a blank *name* is refused, because `apt-get install ""` is a build that fails on a line naming nothing.

## The skip rule is the labels, and only the labels

Each image is `satisfied` when its labels carry every expected key with this binary's values — including the
digest of the program the build would copy — and the sentence otherwise names what differs (`… its pi-version
label says 0.79.1, and this binary's is 0.80.0`, `… its binary-sha256 label says 9c1f…, and this binary's is
4e0a…`). No build timestamp and no image id: a rebuild of identical inputs changes both and would still be
current by any measure the machine can act on. A daemon that cannot answer is *not* an error on this path — it
is `Missing`/`Stale` with what it said — because a read that failed would stop a **dry run** at step 3, and a
dry run's job is to print the plan for the machine it has. The read's signature is `Status`, not
`Result<Status, String>`: every arm is an answer, and a `Result` whose `Err` nothing produces invites a
caller's dead branch (the first draft had one).

## The architecture is checked once, before the first build

The build copies `current_exe()`, never a `dist/<triple>` path, so the payload is always the artifact that is
running. But the **daemon** may be another machine's (`docker_host`), and an `arm64` base with an `x86_64`
static binary inside it is an image whose entrypoint cannot start — a failure that would arrive as a proxy
that will not run rather than as a deploy that refused. So the apply compares `docker version --format
'{{.Server.Arch}}'` (`amd64`/`arm64`) with `HOLOSMITH_TARGET`'s prefix (`x86_64`/`aarch64`) and fails naming
both and the fix. The read does not ask the question: a daemon that cannot answer is "not built", so a dry
run stays a plan.

## A failure names the layer, and nothing is half-tagged

`images::failure` parses the daemon's own shape — the layer (` > [2/2] RUN apk add …:`), the instruction with
the Dockerfile around it (`   2 | >>> RUN apk add …`) and the child's last non-empty line, clipped — and
answers one sentence that says **no new image was tagged**. Measured: a failed `docker build -t <tag>` leaves
no image under a tag that was new, so "does not leave a half-tagged image" is the daemon's behaviour. The
sentence says "no **new** image" rather than "nothing was tagged", because the other reachable state is a
tag that already pointed at an older image: a failed rebuild leaves that one in place, and a sentence claiming
the tag is empty would be a lie an operator could catch with `docker image ls` (it did, in review). The tests
assert `docker image inspect` answers "no such image" after the deliberately broken build into a fresh tag,
rather than trusting the claim.

## The proxies stop mounting the binary, and run as the key store's owner

`EgressProxy`/`KeyProxy` used to run *this* artifact mounted read-only at its own host path, which is why a
`serve` whose binary sits on a tmpfs the daemon cannot see started no proxy at all (ADR-0045). They now run
`holosmith-proxy:<version>` with `--entrypoint /usr/local/bin/holosmith` and **no** bind of the host's binary:
the binary inside the image *is* this release's, which is the identity rule the mount existed to satisfy. The
key store's directory mount stays, because that is data rather than code. `proxies::start_egress` and
`start_key` therefore ask `Containers::image_exists` instead of `is_file`-ing the binary, and answer a sentence
naming the image and `holosmith server deploy` when it is absent — a boot without it warns once and reaches
nothing, before it warned for a different reason.

The key proxy also gained `--user` — the **owner of the key file** — and that is not tuning. The store is a
0700 directory and a 0600 file belonging to the user `serve` runs as, and the container is
`--cap-drop ALL`, which takes root's `DAC_OVERRIDE` with it: a container running as uid 0 cannot read the
machine's own file, fails to prepare the store it can see, exits 1, and `--restart unless-stopped` loops it.
The **first gate run of this story measured exactly that**, and the reason it had never shown up before is the
decision this replaces: the old proxy image was the agent image, whose base user happened to be uid 1000. An
identity that is an accident of a base image is not an identity. Taking the owner from the file makes it a
fact, and `start_key` **refuses** when the file's owner and the mounted directory's owner disagree, because no
single `--user` can then be right and the operator is the only one who can choose. The uid is asserted against
the *invoking* user in the gate, not against `1000`: the invariant is ADR-0035's (the host can commit what the
container wrote), and 1000 is only this sandbox's spelling of it.

## `doctor` compares the labels, in the finding it already had

The acceptance's "so `doctor` can compare them" is new behaviour and it belongs in the existing `image`
finding rather than in a second one: one check word per fact (ADR-0028, ADR-0054). After the pi check passes,
`image_check` reads the configured `agent_image`'s labels — against an expectation built from *this* binary's
bytes, so the digest is compared too — and has four arms:

- labels equal to this binary's expectation → **pass**, naming version, git tag, pi, the leading twelve
  characters of the digest and the toolchains;
- labels present and differing → **fail**, naming every differing key with both values and the fix (a deploy,
  or `agent_image` pointing elsewhere) — which is what makes a stale image a finding rather than a mystery;
- no Holosmith label at all (Docker answers `null`, which `parse_labels` turns into an **empty map** — not
  `None`, which is "the image is not there") → **pass** on the pi sentence, saying the image was not built by
  this binary's image step. This machine's `pi-agent:local` is that arm, and a hard failure there would call a
  supported machine broken;
- the image answered a run and then was not there for the inspect → **fail**, because that is a daemon
  answering two questions inconsistently and it must not pass as "unlabelled".

The labels are read through the same Docker implementation `images::Images` uses, so there is one reader.

## The gate builds, because a case that skips on its own bug is the P1

`case_server_images` is new and runs **first among the cases whose boot starts the proxies** (it is not the
first daemon case — `case_create_session`, `case_worktree` and the delete case precede it and need the daemon
for other reasons), because after this story a `serve` cannot start `hs_egress`/`hs_key` without the proxy
image. It owns its state and **asserts it owns it**: the deploy's own two containers are removed first (a
running container pins the image it was started from, so an untag with them up fails), then both tags, and
`docker image inspect` must answer "no such image" for both before the deploy — because "the image was already
there" is exactly the state that would make its `changed` meaningless. It then asserts the five labels, the
agent image's contents (`pi --version` = `pins/pi.txt`, `git --version`, each configured toolchain, `id -u`
equal to the **invoking** user's and not zero, `holosmith container snapshot --help`), both proxies answering
from the image with **no** binary bind in `docker inspect`, a second deploy saying `satisfied` with unchanged
image ids, and `doctor --full`'s `image: pass:` line naming the labels. It keeps the two images at the end,
because the cases after it boot servers that start proxies from them, and removes only its own containers and
scratch root. Its first build needs outbound network (pi and the toolchains come from two package managers),
which `BUILD_TIMEOUT`'s doc comment says and the case's prerequisite list therefore repeats.

`case_server_deploy` moves with it: its dry run now asserts `3. images: needs:` instead of an owner line, its
partial run stops at step 3 (`failed`, naming the image) because a `_ping` socket cannot build, and its online
half removes both tags and asserts `changed` — the second build is layer-cached. `incomplete: 4 of 7` becomes
`3 of 7`, and step 3 leaves `OWNERS`.

## The broken-Dockerfile arm is a test, not a case

The Dockerfiles are embedded, so a gate could only break a *copy* of one — not the code under test.
`tests/images.rs` drives the production `Images` path at a deliberately broken Dockerfile into a test-only
tag, bounded, and asserts both halves of the acceptance: the sentence names the instruction and the layer, and
`docker image inspect` answers no such image. The same file proves the consts are the files' bytes, the
markers, the `FROM …@sha256:` lines, that neither embeds the pi literal, and `archive`'s ustar shape; its
daemon-gated arms build the real proxy image into a **test-only** tag (`holosmith-proxy:test-images`) so a
test can never collide with a deploy.

## What this story does not do

No multi-architecture build on one machine (`buildx`), no registry push (nothing in phase 2 pushes), no image
signing, and no builder-container self-development path. The skip rule it implements is what
`upgrade`/`rollback` (those stories) consume. Steps 5–7 stay `skipped` on this machine — no systemd, no
`tailscaled` — so a deploy here still ends `incomplete`, and "a fresh host ends with a working HTTPS URL" is
the demo, not this one's.
