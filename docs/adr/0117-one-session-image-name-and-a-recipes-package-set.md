# One session image name, and a recipe's package set is the recipe's

Two config keys existed because there was one image and Holosmith owned all of it.

`agent_toolchains` was a list of Debian packages appended to one `apt-get` line in the agent recipe, carried into the
build as `HOLOSMITH_TOOLCHAINS`, written as the `org.holosmith.toolchains` label, and compared by the deploy's skip
rule. Its own documentation admitted the compromise: the Rust toolchain was excluded from it by hand, "because rust is
part of this recipe, so a machine cannot remove it". A key whose own subject is "everything except the part that
matters" is a key looking for a reader: a project that needs a package already builds its own image from its
`docker/Dockerfile` — `docker build -t <name> …`, named in `base_image` — and a recipe Holosmith reads itself is
planned rather than shipped, so the list has neither a reader nor a subject.

`agent_image` was the machine's one image name, defaulted from `images::agent_image()`, and it was also the gate's
escape hatch: `HOLOSMITH_TEST_IMAGE` and a hand-tagged `pi-agent:local` are how a development machine and
`scripts/verify.sh` point a boot at an image built by hand. The hatch has to survive, so the key does not disappear —
it changes subject from "what a session runs" to "the image Holosmith owns and projects extend".

**Supersedes ADR-0057's** toolchain label: the labels are still the read and the recipes are still embedded, but the
set is four and the toolchain list is not one of them. **Supersedes ADR-0105's** `agent_toolchains` sentence: the fixed
set is still fixed, and there is no configurable half beside it. **Supersedes ADR-0114's** configured `agent_image`:
the second session image is now the one the key names by default.

## D1 — the key is `base_image`, and it defaults to the generic image

`Config::base_image` replaces `agent_image` and defaults to `images::base_image()`, `holosmith-base:<version>`. So a
fresh machine's sessions run the generic image (ADR-0114), and `agent-image:<version>` becomes what it always was
underneath: this repository's own choice, which a machine whose projects are this tree names explicitly. A value that
is not the name step 3 builds is still not a failure — a deploy says so in step 3's line, in the words of an image
Holosmith owns rather than of a session.

## D2 — `python3` and `ripgrep` are part of the recipe

The list's default was `["python3", "ripgrep"]`, and `python3` was not a convenience. `scripts/verify-in-container.sh`
runs the agent image as the gate's own machine, and `scripts/verify.sh` asks `command -v python3` before every case
that speaks a socket or reads a database by hand; a missing `python3` turns each of those measurements into a `SKIP`,
and a `SKIP` measures nothing. Deleting the key with nothing in its place would have made the in-container gate quietly
run fewer cases — the failure this repository already warns about, arriving where nobody would look for it.

So the two packages move into the recipe's **fixed** apt line, which is the status the Rust toolchain already had:
part of the recipe, not configurable. `ripgrep` has no reader anywhere and moves with `python3` rather than being
separated. The image's contents are unchanged, and every `command -v python3` guard still measures.

## D3 — there is no toolchain label

`org.holosmith.toolchains` goes with the list. A recipe's package set is a literal in the recipe, so there is no value
a machine could change for a label to describe, and an image built by a release whose recipe differed is stale through
`org.holosmith.binary-sha256` — the recipe is `include_str!`ed into the binary, so it moves that digest. Every image
now carries four labels, and `case_server_images` asserts the absence of the toolchain label rather than its value.

## D4 — a removed key is refused, and the answer is written down

No hint mechanism was added. `config/schema.rs`'s walk already answers `config.toml:1:1: agent_toolchains: unknown
key`, then `1 problem in config.toml`, and every command that loads a config treats a problem as fatal. `docs/config.md`
keeps a row for each removed name — `agent_toolchains` says a package goes in an image the operator builds from a
project's `docker/Dockerfile` and names in `base_image`, and `agent_image` says it is `base_image` — so searching
either name finds the answer.

## What is not measured

- **A session running in `base_image`.** `case_server_images` inspects `holosmith-base:<version>` — `pi --version`,
  `git`, the isolation probe's `node -e`, and the absence of `cargo` — and no case starts a session in it: every turn
  the gate measures is the agent image's. A session's provisioning against the generic image stands on ADR-0114's
  contract rather than on a measurement of this change.
- **The image a project builds for itself.** Nothing in this tree reads, builds or names a project's
  `docker/Dockerfile`: the operator's `docker build -t <name> …` is theirs, on their machine, and the by-hand image the
  gate already runs sessions in (`pi-agent:local`) is what stands in for one. The mechanism that would read a project's
  recipe is planned rather than shipped.
- **The refusal at a boot and at a promotion.** The walk's sentence is pinned by a unit test and fired by every command
  that loads a config, but no case boots `serve` against an installed file that still names the old key and reads its
  refusal, nor promotes over one.

## Consequences

- **The rename is breaking, and the failure is at the load.** An installed `holosmith.toml` that still names
  `agent_image` or `agent_toolchains` is refused by the walk — `config.toml:1:1: agent_toolchains: unknown key`, then
  `1 problem in config.toml` — and every command that loads a config treats a problem as fatal, so `serve` will not
  boot until the key is renamed. A promotion over such a file fails its health poll and rolls back, and the poll says
  the new release did not answer: the key's name is in the deploy's own log, not in the poll's line.
- A fresh machine's sessions run `holosmith-base:<version>`, and a machine whose projects are this tree names
  `agent-image:<version>` in `base_image`. Step 3 builds both, so neither value draws the remark its line reserves for
  a name it does not build.
- A stale image is still caught: the recipe is `include_str!`ed into the binary, so a release whose recipe differed
  moves `org.holosmith.binary-sha256` and rebuilds, which is what the toolchain label used to be asked for.
- `docs/config.md` keeps both removed names in "Keys that are gone", and the two recipes' headers say where a
  project's own package goes — an image the operator builds, tags and names in `base_image` — so a reader arriving with
  an old key finds one answer.
