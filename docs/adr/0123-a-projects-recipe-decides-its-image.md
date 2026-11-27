# A project's own recipe decides the image its sessions run

Every session ran the image one config key named. `ContainerSettings::image` was `base_image`, and
`SessionContainer::of` copied it into `docker run`'s image argument, so a machine had one session image and a project
had none. ADR-0117 stopped at exactly that line: a project that needs a package builds its own image and names it in
`base_image` — one image per *machine*, so a second project needing a different toolchain had no answer at all.

A session row knows its project, and its project is a directory under the projects root, so the image can be the
project's: `hs-<project>:<digest>` when the project carries a recipe, and `base_image` when it does not.

## D1 — the recipe is `<project root>/docker/Dockerfile`

It is a project's own file in a project's own repository, so it is reviewable in the same diff as the toolchain it
installs, and `<project root>` is already where the worktree's repository lives. The recipe is read from the project's
own checkout under the projects root and never from a session's worktree, so a session's own edit to it takes effect
when it is merged — the image is the project's, not a branch's. A second location (`<root>/Dockerfile`)
would put a recipe where a project's own tooling does not expect one, and a name in a config key would make the recipe
something an operator configures rather than something a repository carries.

**Rejected: a config key naming the image** (`base_image` with a per-project override). It would be a second source of
truth for what a project needs, kept on the machine rather than beside the toolchain, and the machine would have to be
edited whenever the recipe changed.

## D2 — the tag is a content address over the recipe and the base's tag

`hs-<project>:<digest>`, where the digest is the full SHA-256 of the recipe's bytes **and** the base image's tag
(`images::base_image()`, or whatever `base_image` names), length-prefixed so the two inputs cannot be misread as one
string. The currency question is then "does that tag exist?" — one `docker image exists` read, and nothing else. A full
digest is 64 hex characters, inside Docker's 128-character tag limit, so nothing is truncated; a shortened digest would
be a second thing an operator had to be told.

The base's tag is in the digest because a recipe's `FROM <base>` is only half of what an image is: a base that moved
rebuilds every project's image from that base, and a digest over the recipe alone would leave the tag identical.

**No `org.holosmith.*` label is written or read for a project's image.** The four labels are a claim about an artifact
whose recipe *Holosmith* has read — Holosmith stamps them on the proxy and the base image and on nothing else — and a
label on a project's image would be a claim beside a tag that already says what the image is. It would also make the
question "is this built?" a label parse rather than an existence check.

**What the digest does not cover is the files the recipe copies.** A `COPY Cargo.lock web/package-lock.json …` over a
lock that has moved is invisible to both inputs: the tag is the same, the image it names is stale, and no read says so.
That is deliberately not this change's problem — the alternative, a digest over the tree, moves on every `cargo build`
and is not a content address of anything an operator wrote — and the change that closes it owns it.

## D3 — the build context is the project root, filtered by `.dockerignore`

The context is the directory Docker is handed, and it is the project root: a recipe has to be able to `COPY Cargo.lock`
and `COPY web/package-lock.json`, which is the whole reason this milestone exists. `docker/` alone as the context would
make every project duplicate the lock files it vendors against — a second source of truth for the vendoring at the
moment this work removes the first. `.dockerignore` is a convention an operator already knows, and the resolver does not
parse it: the answer is *which directory*, and Docker reads the project's own filter when it has one. An absent
`.dockerignore` is not an error.

The resolver returns the tag, the recipe's path and the context in **one** value
(`projectimage::ProjectImage`/`Build`), so `SessionContainer::of`, `doctor`, the build and the `image build` verb that
follows cannot disagree about either. Nothing else in the tree composes `hs-<project>:<digest>`.

## D4 — a project with no recipe runs the base image, and `base_image` stops being the answer

`SessionContainer::of` resolves the image per project, and `ContainerSettings::image` becomes what it always was
underneath: the **base** image's tag, which a project's recipe extends and a project without one runs.

A project with no recipe is **supported, not refused**: it runs `holosmith-base:<version>` — pi, git, node and
certificates — and a session there can commit, ask questions and edit files; it cannot compile. "Create the project,
then write its Dockerfile" is the path this milestone exists to make easy, so `doctor` says what such a project's
sessions cannot do instead of a create refusing them.

## D5 — a recipe that cannot become an image is refused at session start

A `docker/Dockerfile` that is a directory, a symlink to nothing, empty (zero bytes or whitespace only) or unreadable is
refused **before the row exists**, through the same refusal path the repository's mount rules use: `Backend::refuse_project`
is asked by the create and the resume, and `SessionContainer::of` — the one builder — answers the same refusal so a
project that changed under a running session is caught too. The sentence names the path, so the operator's next step is
an edit in that file, and it arrives as `422 unusable_image` rather than as a `docker run` failure three steps later.

A project name Docker cannot spell as a repository (`MyRepo`, `.repo`, `-repo`, a 253-character name) is the same arm
when a recipe is present: Docker's grammar is lowercase alphanumerics separated by `.`, `_`, `__` or a run of `-`, and a
project name is a wider alphabet than that (`projects::name_is_valid` accepts all four of those). It is refused rather
than lowercased, because `MyRepo` and `myrepo` are two projects and one tag for both is one project's image run by the
other's sessions.

## D6 — `doctor --quick` gains one information per project

`doctor` gains a fourth word, `Check::Project` (`project`), and a fourth outcome, `Outcome::Information` (`info`): the
ticket's word is "information", and `unknown` already means something else — "this check produced no verdict", whose
sentence says what was missing. An information is a fact and not a verdict, and it is counted in the tally line
(`doctor: 11 findings: 9 pass, 0 fail, 2 unknown, 0 info`), which the gate greps for its prefix. It is never a failure:
`Report::failures` stays `Fail`-only, `doctor`'s exit code is unchanged, and `serve` refuses nothing over it.

One finding per project, sorted by name, in **both** depths, because a project's image is what the next create is about
to run and `--quick` is the list `serve` refuses on and the unit's `ExecStartPre`:

| state | outcome |
| --- | --- |
| no `docker/Dockerfile` | **info** — its sessions run the base image, which has no compiler |
| a recipe that cannot become an image | **info**, in the recipe's own sentence naming the path — the create refuses, the report does not |
| a recipe, and the daemon did not answer (or this is a hub) | **unknown**, in the `because` voice the container questions already use |
| a recipe, and its image is there | **pass**, with the tag |
| a recipe, and its image is not there | **info** — it will be built on the first session |

The image question is asked through `doctor::Machine` (`image_exists`), so a test answers it without a daemon, and a
projects root that does not exist has no projects and therefore no line — the first `project add` makes the directory.

## What is not measured

- **A session running a project's image.** Nothing builds `hs-<project>:<digest>` yet: the build is its own change, so a
  session on a project whose recipe is present and whose image is absent fails the container step at the pi-version
  probe — `the image <image> could not say which pi it runs: <docker's words>`, the daemon's pull error inside it —
  rather than starting. The `info` sentence above describes the behaviour that change gives it, and says "will be built" for
  that reason. Every turn the gate measures runs the base image, because no gate project carries a recipe.
- **A stale copy.** The digest cannot see a `COPY`'d file move, by D2's design. A case that proved it would be a case
  about the excluded thing.
- **The recipe's own correctness.** A Dockerfile that is a file but not a valid build is the daemon's to refuse, and
  nothing here reads Docker's grammar for a recipe's body.

## Consequences

- `SessionContainer::of` returns a `Result`, so every caller decides what to do with a project whose recipe cannot
  become an image: the create answers it, the container step fails with the sentence, and a teardown does not care
  because a container's **name** is the naming rule's and not the image's.
- The pi-version assertion is per image rather than per process: a session's image is its project's, so one project's
  answer is no fact about another's. A project whose recipe installs a different pi is refused as a stale image where
  before it ran the base's.
- `docs/config.md`'s `base_image` row and its removed-key table, `AGENTS.md`'s offline-workflow paragraph and
  `GLOSSARY.md`'s **Session container**, **Refused project**, **Finding** and **Image build** entries say this instead of
  what they said, and **Project image** is the entry this decision adds.
