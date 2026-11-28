# A project's image can be run in a session's shape, and the shape has one source

A project that wants to check in its own CI that its image works has to reproduce a session's conditions by hand: no
network, `--cap-drop ALL`, `--security-opt no-new-privileges`, `--init`, `--rm`, the three limits, and
`--user <the data root's owner>`. Those flags live in exactly one place today, `container::SessionContainer::run_args`,
and the one everyone gets wrong is `--user` (ADR-0087): root passes, the image's own user passes, and the invoking uid —
the identity a session actually uses, which may have no passwd entry inside the image — is the one that shows whether
the image can write the tree it is given. A hand-rolled copy in every project's CI reintroduces that trap, and nothing
notices.

So the verb, and a value type behind it: **`holosmith image run <project> -- <cmd>` runs one command in the image the
project's sessions run, taking its flags from the same value a session's container does.**

## The decision, in a line

`container::SessionShape` is the lifecycle, the hardening, the limits and the identity as one value; `run_args` on a
session's container and `run_args` on an `ImageRun` both consume it. `holosmith image run` resolves the image the way a
session does, builds the tag when it is absent under the same lock and into the same log, prints the `docker run` line
it used, runs it with `--network none` and one writable mount at `/work`, and exits with the command's own status.

## D1 — the flags have one source, and it is a value, not a function

`SessionShape { limits, user }` has four producers — `lifecycle`, `hardening`, `limit_flags`, `identity` — and
`arguments`, which concatenates them for a caller that has no flags of its own between them. The split is four rather
than one list for one reason: a session's vector interleaves its own `--name`, `--network`, `--env` and `-v` flags at
particular places, so a single block would have meant reordering a session's argv. **A session's argv is unchanged byte
for byte**, and that is a constraint rather than a preference: the vector is what `tests/container.rs` measures against
a real container and what an operator reads in a launch line.

What the split costs is an ordering that each caller owns. What it buys is that neither caller can spell a shared flag:
the only producer of `--cap-drop ALL` is `SessionShape::hardening`. The unit test in `container.rs` pins each block as a
literal and asserts that **both** vectors carry every block whole and adjacent, so a change to one caller that is not a
change to the value fails there rather than in a project's CI months later.

The session's own half stays the session's, and the test says so: `--name`, `--env`, `--detach` are asserted **absent**
from the run's vector, and a session's private `agents_<id>` network, its five mounts, pi's environment and the launch
command stay in `SessionContainer`. A build or a CI run wants none of them.

## D2 — `--network none`, and one mount, and the project's checkout

A session's network is `--internal` so that the key proxy and the egress proxy are its only routes out. A CI run has no
proxy to reach and no model to call, so `--network none` is the stronger statement of the same fact, and
`network_name()` is deliberately not reached for: there is no network to create, name or clean up.

The verb mounts **one** directory, writable, at `WORKDIR` (`/work`): the project's checkout at
`<projects root>/<project>`, or `--work DIR`. Deriving `Mounts` wholesale would put a session's `.git`, its read-only
hooks and config, and a `pi-home` into a CI run, so the single mount is built directly and goes through the same
`Mount::argument` producer as the five a session has. A `--work` that is not a directory is **refused**, because
`docker run -v` creates a missing source as an empty root-owned directory rather than failing (ADR-0035).

The identity is `owner_of(&settings.data_root)` — the read ADR-0087 puts behind `--user` — reached through
`SessionShape::of`, so a session and a run on the same machine name the same uid, and `None` leaves the image's own
user, the same fallback a session has.

## D3 — the image is the resolver's, the build is the create's, and the refusals are exit 2

The verb does not compose `hs-<project>:<digest>`: `projectimage::resolve` is still the only place that does, and a
project with no recipe runs the base image exactly as its sessions would. The build is `imagebuild::ensure` with
`Currency::Missing` — the same call a create's container step makes — so there is one currency rule, one machine lock,
one per-image lock and one `<data>/images/<repository>.log`, and a verb that finds the tag current builds nothing.

Refusals are **exit 2 and one sentence**, in the order the questions get cheaper and more local: no command after `--`,
a project name no create would accept (the create's own `projects::name_is_valid`, asked once for both verbs, because
the name becomes a directory and `run` mounts it writable), a project that is not under the projects root (with `image
build`'s own sentence, because it is the same fact), a recipe that cannot become an image (the create's `Unusable`
sentence), a `--work` that is not a directory, and a daemon that does not answer (`preflight::daemon`, the machine's one
`_ping` read, rather than a second probe).

A **failed build is not a refusal** and keeps `image build`'s two codes: exit 1 with the tail and the log's path, and
exit 4 when a pinned fetch did not answer (ADR-0111). Nothing about the invocation was wrong in either case, and a
caller that read a host's unreachable musl.cc as a bad verb would record a refusal nobody made.

## D4 — the line is printed on stderr, and the command's stdout is the command's

The verb prints the `docker run` line it used, so a project author can paste it into a pipeline that cannot have
holosmith. It goes to **stderr**, with the verb's `holosmith: image run: ` prefix: stdout belongs to the command, and a
`holosmith image run p -- cat f > out` that captured the line as well would be a verb that cannot be used in a pipeline
at all. The line is `docker.line(&arguments)` rather than a second rendering: the same `-H` the spawn uses, from the
same field, with every word shell-quoted by `remote::quote`, so a path with a space in it stays one word.

The command's own status is the verb's: its exit code, or 128 plus the signal that killed it, which is what a shell
reports for the same ending. A command that exits 2 therefore exits 2 through this verb too: only the `image run:`
sentence on stderr and the absence of a printed line tell a caller which happened. The refusals above are the only way
the verb exits 2 *itself*.

## D5 — the gate's case is separate, and it is the verb's own

`case_image_run` runs the verb against a fixture project whose recipe is a comment plus `FROM holosmith-base:<version>`,
with the base `case_server_images` leaves in place, and asserts the printed line's flags, the build's log under the
config's data root, the identity and working directory the command saw, the command's own exit status (7, not 2), and
that `--work` really moves the mount. The recipe's first line carries the gate run's pid, because the tag is a content
address and two gate runs on one machine would otherwise share one image and each remove the other's.

It may not be folded into `case_session_build`, and that case may not use this verb. A case that proved the shape
**through** our own verb would be partly testing itself — the shape's facts would hold by construction and the case
would fail only if the verb and the container both broke in the same way. `case_session_build` keeps measuring the
shape with hand-written `docker run` arguments, and this one measures the verb.

## Refused

**A documented `docker run` line and nothing else.** Each project gets `--user` wrong and nothing notices. The line
still exists, as this verb's own output — it just stops being the only answer.

**"Use a session instead."** A session needs a model and a provider key, so it cannot be a CI step.

**Nothing.** Leaves the trap in place and makes it every project's private discovery.

**A second build path.** `docker build` in the verb, or a `Currency` of its own, would be the second currency rule
ADR-0124 already decided against; the verb calls `imagebuild::ensure` and reads the resolver's tag, and nothing else.

## Consequences

- A project's CI can run one command in the image its sessions will use, as the identity a session uses, and paste the
  line into a pipeline that has no holosmith.
- `container.rs` is the one place a session's shared flags are spelled, and the unit test there fails if either caller
  drifts from it.
- Nothing in a session's own `docker run` changes: the extraction is byte-identical, which is what the existing argv
  tests still assert.
- `docs/config.md` shows the printed line, and `AGENTS.md` says the verb exists and what it is for, so no project
  author has to discover the shape by failing at it.
- The gate gains one case of a few seconds, registered after `case_session_build` so the base image it builds `FROM` is
  already there.

## What is not measured

- **A project whose session image is not the base.** The gate's fixture runs `FROM holosmith-base`, and the verb's own
  behaviour does not depend on what the recipe installs: what it measures is the container the command runs in.
- **A uid the image has no passwd entry for, through the verb.** The gate case's run uses the invoking uid, which on a
  host whose uid is the image's own user is not the passwd-less case; that identity's facts are `case_session_build`'s
  two stranger arms, which is where the measurement belongs. The verb names whichever uid the data root's owner is.
- **The daemon's own view of a `--work` path.** A directory on a tmpfs the daemon does not share is refused only when
  it is not there at all; that a path exists and is a directory is what the host can see, and ADR-0035's empty-mount
  failure is a fact about the daemon's namespace that no read here can measure.
