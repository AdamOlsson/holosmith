# A project's image is built on the first session that needs it

ADR-0123 gave a project's sessions their own image: `hs-<project>:<digest>` when the project carries
`docker/Dockerfile`, and `base_image` when it does not. Nothing built it. `server deploy`'s step 3 builds the three
images Holosmith owns, and a project's recipe arrives *after* the project does, from a person, in a git commit — so a
machine that never ran a deploy, or whose tags a `docker rmi`, a failed deploy or the gate's own image case removed,
met a missing image at the pi-version probe: `the image hs-foo:<digest> could not say which pi it runs: … pull access
denied`, three steps into a session that was already `starting`.

So the container step builds what `docker run` needs, before it runs anything, and the row says what it is doing.

## D1 — the first session that needs the image builds it, under a lock

The trigger is an existence check: `hs-<project>:<digest>` is a content address, so "is it built?" is one
`docker image inspect` and nothing else. Absent, the container step builds it — `docker build -t <tag> -f <project
root>/docker/Dockerfile <project root>`, the project root as the context so the recipe's `COPY Cargo.lock …` works and
the project's own `.dockerignore` filters it, and `-f` because Docker reads `Dockerfile` from the context's own root
(ADR-0123). No `--build-arg` is passed: a project's image carries no Holosmith label, and an argument a recipe does not
declare is a BuildKit warning over nothing.

**Two sessions created a second apart must not run the same `docker build`**, and the analogue is ADR-0037's
per-session start lock — but that lock is a map in this process, and `holosmith image build` is a *second* process
which cannot see it. So both locks are **files under the data root**, and both are exclusive-create with an identity
in them: `<pid> <starttime>`, the pair the gate's own cleanup compares. There is no `flock`: `unsafe_code` is denied,
the crate set is the image's, and a crate for it would be a rebuild of the image that vendors them.

- `<data>/images/locks/<repository>.lock` — one per project's image: `hs-foo.lock`.
- `<data>/images/locks/machine.lock` — one for the machine, held across the whole ensure.

The base takes no lock of its own: it is one tag for the machine, and the machine's lock is what serialises it, so
there is no second `holosmith-base` build for a per-image file to hold off. An operator looking for a stuck build
there finds `machine.lock`, and an `hs-foo.lock` only for a project's own.

The machine's lock is taken first, so a project's build waits on the base's instead of racing it, and the machine runs
**one build at a time**. That cap is a constant in this change and not a config key: builds are the largest thing this
program ever writes to disk, and a machine that ran four of them at once would be a machine whose sessions all got
slower for it.

**A lock that outlives its holder is "this project can never start a session again"**, which is worse than the
duplicate build the lock exists to prevent, so a holder is detected rather than trusted. A lock whose pid is not
running is stale; so is one whose pid is running under a *different* start time, which is a number the kernel has
since handed to something else; so is one that has said nothing and is older than five seconds, which is a writer that
died mid-line. Whoever finds a stale lock removes it and tries again. A file with no identity yet is the one case that
waits rather than decides — its creator has made it and has not written the pid — and five seconds is longer than a
scheduler pause and shorter than anything an operator notices. The removal on the way out is guarded by the same
identity, so a holder that lost its lock to a breaker does not take the next holder's file away.

## D2 — the base image is ensured by the same step, before a project's

A project with no recipe runs `base_image`, and a project with one builds `FROM` it, so a machine that never ran a
deploy meets a missing base before it meets a missing project image. The base is the cheaper build and the one a recipe
depends on, and ensuring it first is what makes a project's build a single attempt rather than one that fails inside
`FROM`. So the step asks "does it exist?" twice, in that order.

The base's build is not the project's build:

- **Its context is empty.** `Kind::Base.dockerfile()` is embedded bytes and `Kind::Base.context()` is the zero-length
  slice (ADR-0114), so there is no project directory, no `.dockerignore` and no digest to compute: the tag is
  `images::base_image()` and "the tag exists" is the whole test.
- **It is the recipe `server deploy` builds** — the same `images::ensure`, over the same embedded bytes, with the same
  `Expectation` and the same architecture guard, so a machine cannot end up with a base whose labels or build
  arguments came from a different path than a deploy's.
- **It is ensured only when the config's `base_image` names it.** A `base_image` naming another image — this
  repository's `agent-image`, or a hand-built `pi-agent:local` — is not a recipe Holosmith has read, and a build here
  would be this program guessing what an image it has never seen should be. `server deploy`'s step 3 builds those, and
  `doctor`'s `image` finding already names the fix when one is missing.

`server deploy` keeps building the base, unchanged: this is the repair for a machine that has no base, not a
replacement for the install step that normally provides one.

## D3 — a bound, not a timeout to fail by

`images::PROJECT_BUILD_TIMEOUT` is an hour and the base keeps `images::BUILD_TIMEOUT`'s forty minutes. A project's
first build is slower than any of the three product images — this repository's installs a Rust toolchain, runs
`cargo vendor` over the lock and `npm ci` over the web app — and the number is the product images' with room on top,
because the one build this repository can time is not the build a project's author wrote.

`Ok(None)` from the build is "the build did not finish", which is a sentence rather than a wedged session. The two
contexts do not fail the same way and do not say the same thing: an archive's bound is about pi, the two bases, the
crate set and the cross toolchain, and a directory's is about whatever the recipe fetches.

## D4 — the output lands in two places, and the failure names both

The session's `status_reason` carries `images::failure`'s sentence — the layer, the instruction and the child's last
line, clipped — so an `apt-get` error is visible without hunting. The whole of the build's output is
`<data>/images/<repository>.log`, and the failure's sentence names that path.

One file per **image** and not per tag: a tag's digest moves on every recipe edit, so `hs-foo:<digest>.log` would be a
file per edit while `hs-foo.log` is the log of the last build of `hs-foo`, whatever digest it was under. A project
genuinely named `base` gives `hs-base.log`, which is not the base image's `holosmith-base.log`. Each build truncates
its own file, so a second build's failure is not read as the first's.

## D5 — the row's `status_reason` is the progress line

A deploy's progress line is stderr (`crate::progress`), and a session's is not: the operator watching a session is in
a browser, and what that browser and `GET /api/sessions` read is the row. So the container step is handed a
`StepNotes` sink, the provisioner implements it as a write of `status_reason` — the backend may not write a row
(ADR-0019) — and `building hs-foo:…` reaches the row before the build that takes minutes. The reason is cleared when
the step that wrote it is over.

Two things bound that write. It carries `AND status = 'starting'`, because a step can take minutes and another
writer can move the row under it — `pi_exit` puts it `stopped`, a crash `error`, a delete `deleting` — and a build's
sentence on a row whose own reason says why it left (`stopped · built hs-foo:…`) would be a lie about the exit; the
clear that follows a finished step is the same statement and cannot wipe a reason it did not write. And a session that
is **waiting** for a lock says so: the wait is bounded by an hour and a half, so the row says `waiting for another
image build to finish` before it says `building`, which is the row this decision exists to prevent.

A `session.status` frame is deliberately *not* emitted for these: a step frame carries no state change and the app's
step line already says which step is running, so a frame whose `reason` no reader renders would be a claim nobody
reads. The row is where the sentence lives, and the app shows it the next time it reads the row.

That step line is the **session list's** (`applyStatus` renders a step frame's `step`): the session screen's own
`session.status` listener takes a transition and nothing else, and the screen reads no row while it is idle, so an
operator sitting on `/sessions/<id>` sees a bare `starting` badge until the screen next reads the row — which is the
concession *What is not measured* makes two paragraphs down, rather than a second place the sentence is rendered.

## D6 — builds have network, sessions do not

The daemon builds on its own default bridge; the container the build produces joins an `--internal` network. That
asymmetry is real, deliberate, and the first thing an operator with a recipe that needs `apt-get` has to be told,
because the first reaction to "my recipe needs the internet" is "but sessions have no network". It is in
`docs/config.md`'s `base_image` row, in `docs/architecture.md`'s image paragraph, and in the `image build` verb's own
help.

## D7 — `holosmith image build <project> [--force]` and `--base`

A fresh machine should not pay a six-minute build inside somebody's first turn, and an operator who has just written a
recipe wants to know it builds before a session depends on it. So the lazy path has a verb: `image build <project>`
ensures the base and then that project's tag, and `image build --base` pre-warms the base with no project argument.

Both are idempotent on the tag's existence — the digest is the whole currency question — and `--force` is the one way
to rebuild a tag the digest says is current: a recipe whose `COPY`'d file moved is stale with the same tag, and no read
says so (ADR-0123's excluded case). `--force` rebuilds the subject it names and nothing else, so `image build
<project> --force` does not rebuild the base.

The verb's exit codes are two facts and not one. **1** is a build that failed — the tail of what the daemon said on
stderr, and the path of the whole log beside it. **4** is a build whose pinned fetch did not answer, and it is the code
`server deploy`'s image step already answers (ADR-0111): that one is a host that could not reach musl.cc, not a recipe
that will not build, and a caller that read the two as one would record a refusal the tree never earned.

## What is not measured

- **A cap on the bytes a build writes.** Images live in the daemon's storage rather than under the data root, and may
  be on another machine entirely when `docker_host` points elsewhere, so `session_disk_soft_gb` cannot see them and no
  key watches them. `docker system df` is the operator's read, and `docs/config.md` says so beside the two disk keys.
  The concurrency cap of one is what this change offers instead.
- **A stale copy.** The digest cannot see a `COPY`'d file move, by ADR-0123's design. `--force` is the repair and
  nothing measures when it is needed.
- **A second build of the same tag by two machines.** The locks are files under *one* data root. Two machines pointing
  at one `docker_host` have two data roots and two sets of locks, so the cap of one is per machine and not per daemon.
- **The build's own correctness.** A recipe that is a file but not a valid build is the daemon's to refuse, and the
  sentence is the daemon's.
- **The app's live badge.** The reason is on the row; the app renders it when it next reads the row, and a live
  update would be a web change with its own test.

## Consequences

- The container step's first act is the ensure, and the pi-version assertion runs after it: an image the step built is
  an image `docker run --entrypoint pi` can ask a question of.
- A resume ensures the same images with no sink — its ladder has no row of its own to write — so a machine whose tags
  a `docker rmi` removed is repaired by the next resume rather than by the next create alone.
- `images::Images::build` takes an `Input` and an optional log path: the archive-on-stdin shape the three product
  images use, and the directory-on-disk shape a project's recipe needs, with the bound and the "did not finish"
  sentence following the shape.
- `SessionBackend::provision` takes a `StepNotes`, and `sessions::repository::set_reason` is the column write behind
  it — not `set_status`, which the transition table owns.
- `docs/config.md`, `docs/architecture.md`, `AGENTS.md`, `GLOSSARY.md` and ADR-0123's *What is not measured* say this
  instead of what they said.
