# A release is built by the tree's own script, and no recipe of ours builds us

`holosmith build` started a builder container from `crates/holosmith/images/builder.Dockerfile`, which was
`include_str!`ed into the binary like the three product recipes (ADR-0080). That made the builder the fourth recipe in
the artifact, and it put the recipe on the wrong side of the thing it builds: release N was compiled by **binary N−1's**
recipe, so a fix to the recipe — a base's digest, a toolchain's pin, a `RUN` that had to change — landed in the release
after the one that needed it, and nothing in that release's output said which recipe had built it. The verb was also
the only command in this tree whose subject was a container that builds the binary it was running, and it needed Docker
and an existing `holosmith` to do it: a machine with neither could not make a release, and the one shape that could was
the shape the gate already ran.

**Supersedes ADR-0080's** D1 and D2: there is no `holosmith build`, no `holosmith-builder:<version>` image, no builder
recipe in the artifact and no `org.holosmith.builder-recipe-sha256` label. ADR-0080's other decisions — the build is
safe and never trusted, the review is the gate — stand, and they are the half a promotion still enforces.

## D1 — the tree's own `scripts/build.sh` is the release path

`scripts/build.sh` builds the web app and then the static artifact, and leaves `dist/<triple>/holosmith` with
`holosmith.sha256` beside it: `<digest>  holosmith`, the shape `sha256sum -c` reads. That pair is what a promotion
reads, and it is the same pair `releases/<tag>/` holds once the candidate is live.

Where it runs is the caller's business:

- a worker builds with whatever toolchain its turn has — the agent image's, or this host's `~/.pi/toolchains/env.sh`;
- `scripts/verify-in-container.sh` runs the whole gate in that image on this machine's daemon (ADR-0109);
- `holosmith update` verifies the candidate tree by running that gate in the image the running release deployed, and
  promotes the artifact the gate left — so the binary that is promoted is the binary that gate built, and the script
  that built it is the candidate's own.

The reason ADR-0080 gave for the verb — "the build never runs on the host, so a `build.rs`, a proc macro or the web
app's Vite config cannot run outside a container" — is met by that path rather than dropped: the gate runs in a
container, and it is the container's tree, not this binary's embedded copy, that builds the release.

## D2 — what stays in `builder.rs`, and why it is not a recipe any more

`crates/holosmith/src/builder.rs` keeps the rules a **release** needs, and nothing else:

- `BINARY`, `DIGEST` and `PENDING` — the two file names a release directory holds, and the temporary a build writes
  before one of them exists;
- `place`, which is the rule a release directory obeys: a built artifact moves in, a *different* binary under the same
  name is refused, and the digest file is written beside it. Nothing calls it — `release::Release::write` is the one
  **writer** of a release directory and implements those three outcomes itself — so the function and its `Placement`
  are kept as the statement of the rule, measured by `crates/holosmith/tests/build.rs`;
- `artifact_problem` and `default_target`, which read an ELF header for the machine it is for and the interpreter it
  must not name — `e_machine`, `ET_EXEC`, no `PT_INTERP` — so `holosmith smoke` and the promotion itself can refuse a
  candidate that is not this machine's static build.

Two container helpers stay with them because `holosmith smoke` is a second caller and has been since ADR-0081:
`daemon_problem`, one sentence for a machine whose daemon did not answer, and `tail`, the end of a child's output
rather than its head. The module is a release-directory module now, and a reader who opens it looking for a Dockerfile
finds its opening paragraph saying where the artifact comes from instead.

Promotion is otherwise untouched, and it already guarded what the verb used to: the candidate's ELF machine, its
digest against the file beside it, and a refused overwrite in a release directory. Those guards were never the verb's.

## D3 — what a reader of ADR-0080 should now do

ADR-0080 stays as the dated record of a decision that has been reversed; it is not edited. Read its D3–D6 for the
container's shape — `--rm`, the read-only `/src`, the output directory as the one writable mount, `HOLOSMITH_GIT_TAG`
— as history: the shapes those paragraphs justify are `scripts/verify-in-container.sh`'s and the agent image's now. Its
D2's recipe arguments (named stages, no `CARGO_HOME`, pins by digest) are the product recipes' arguments, and they are
still measured there.

A machine that used `holosmith build` to make a release artifact runs `scripts/build.sh` from the tree it is releasing,
in the agent image if it has one: `docker run --rm --network none --user "$(id -u):$(id -g)" -v "$PWD:/src" -w /src --entrypoint /src/scripts/build.sh agent-image:<version>`.
The `--user` is not optional: the image ends `USER node` and this command writes `target/`, `dist/` and
`web/node_modules/` into the mounted tree, so on any other account the build stops on a permission — it is the shape
`case_agent_offline_build` measures (`scripts/verify.sh`), which also passes a session's `--cpus 2 --memory 4096m`.
The two files it leaves are the two a promotion reads, so nothing downstream changes.

## What is deliberately not measured

- **Nothing replaces `case_build`.** What that case measured was the deleted command's Docker plumbing — a `docker`
  stand-in's recorded argv, the recipe's digest, the two mounts, the placement and the cross-machine refusal — and with
  the command gone there is no argv to record. The placement and the ELF refusals it also covered are still measured
  where they live now: `crates/holosmith/tests/build.rs` over real files, and `case_upgrade`/`case_smoke` over a real
  candidate.
- **A real musl.cc download.** It needs the network and is not a gate case (ADR-0111, ADR-0120). The tarball's
  SHA-256 in the recipe is the guard: a download is a pin only when its bytes are checked, and the check is a `RUN` no
  edit can quietly drop without `tests/images.rs` failing.
- **A release artifact's build in the gate.** `case_agent_offline_build` already builds this tree's artifact inside the
  agent image with `--network none`, and it is the measurement of the surviving path; a second one that ran
  `scripts/build.sh` on the host would measure what a worker's own turn measures.

## Consequences

- The artifact carries three **product** recipes and the smoke recipe's, not the builder's: `scripts/check-static.sh`
  greps the built binary for `proxy`, `base` and `agent`, and it greps three because the smoke recipe is not what a
  deploy ships. The fifth — the builder recipe, and the marker it left — is gone.
- A recipe fix reaches the release that needs it, because the recipe is the candidate tree's and the candidate's own
  gate built the artifact it promotes.
- The gate is one case shorter (`case_build`), and `docs/gate.md`'s counts and this run's total are adjusted by it
  rather than by a re-measurement: the case's cost is known, the rest of the run is unchanged.
- `GLOSSARY.md` loses **Builder image** and restates **Release build** as the pair `scripts/build.sh` leaves, so a
  reader searching the old name finds nothing rather than a description of an image nothing builds.
