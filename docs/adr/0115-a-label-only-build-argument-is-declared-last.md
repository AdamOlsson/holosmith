# A build argument nothing but the `LABEL` reads is declared last

The gate is documented as taking 34 to 43 minutes when its image cases rebuild `agent-image`, and the stated cause
was that `case_server_images` removes both product tags at its start and the rebuild is what follows. That cause is
wrong. Removing a tag leaves the build cache alone, so the rebuild after it is nearly free; the cost was the **scope**
of three build arguments.

Measured on this host, Docker 29.9.2 with the containerd image store:

| measurement | result |
|---|---|
| `docker image rm -f agent-image:0.1.1 holosmith-proxy:0.1.1`, then rebuild with the same facts | 0.73 s, all 16 layers `CACHED` |
| build cache before and after that removal | 153 entries, 19.47 GB — unchanged |
| rebuild with one changed `HOLOSMITH_GIT_TAG` and nothing else | **every** `RUN` busted: apt 42–64 s, `rustup component add` 2.8 s, `rustup target add` 16 s, then both pinned musl.cc fetches, the pi install, `cargo vendor` and `npm ci` |
| the same Dockerfile with the three label-only `ARG`s moved below the last layer, then version, git tag and binary digest **all** changed | 1.2 s, every `RUN` `CACHED` |

The rule behind the third and fourth rows is BuildKit's: a `RUN` is in scope of every `ARG` declared above it, and
that scope is part of the instruction's cache key whether or not the instruction names the value.
`HOLOSMITH_VERSION`, `HOLOSMITH_GIT_TAG` and `HOLOSMITH_BINARY_SHA256` sit at the top of `agent.Dockerfile`,
`proxy.Dockerfile` and `base.Dockerfile` and are read by nothing but the `LABEL` block at the bottom: they are three of
the facts a deploy's skip rule and `doctor` compare, and the binary's digest is the label that makes the rule about the
*binary* rather than about a tag (ADR-0057).

**Why that was every run and not an occasional one.** A worker's tree always changes two of the three: the git tag
describes a commit that has moved, and the binary's digest is a different file. So every gate run invalidated the
toolchain, both pinned musl.cc tarballs (ADR-0111), the pi install and the vendored crate set — for values nothing
above `LABEL` reads. The rebuild's cost was attributed to the one step that made it visible rather than to the
arguments that caused it.

## D1 — A label-only argument is declared under the last `RUN`, and each recipe says why

All three session recipes declare their label-only arguments immediately above the `LABEL` block, below every `RUN` and
the `COPY` of the binary. `agent.Dockerfile` and `base.Dockerfile` keep `HOLOSMITH_PI_VERSION` where it was, because
the pi install *spends* it: its value changes what the image holds, so a change to it should cost the layers below.
`proxy.Dockerfile` moves its whole block, since its single `RUN` names none of the four. The rule is per recipe and the
base image is the third: `server deploy`'s step 3 builds it on every machine, so leaving it out would keep the exact
cost this decision removes.

**The recipes say where a spent argument goes, and it is not the same sentence.** The rule a recipe states is "a
label-only argument is declared under the last `RUN`; a spent one is declared above the step that spends it" — not
"every argument is declared here and every step below spends it". `agent.Dockerfile`'s pi version is the example: it
sits above the `apt-get` line, the two `rustup` layers and both musl.cc fetches, so a pi bump rebuilds them on the way
to the install. That is the price of declaring it where it is, the file says so rather than claiming the layers above
the install read it, and moving it is a separate decision.

`builder.Dockerfile` already had this shape — `ARG HOLOSMITH_BUILDER_RECIPE` sits directly above its `LABEL` — and
`smoke.Dockerfile` declares no argument at all, so neither changed.

The rule is written beside the arguments in all three recipes rather than only here, because the next fact an image
needs (a registry, a build date, a source URL) will be added at the top of the file where the others were, and the
reason it must not be is not visible from a `LABEL` six hundred lines down.

**A daemon-free test is the half that keeps it.** `tests/images.rs`'s
`a_label_only_build_argument_is_declared_below_every_run` reads the embedded recipes and asserts, per recipe, that
every argument its `LABEL` block writes and no `RUN` reads is declared after the last `RUN` — the failure sentence
names the cost, so a reader who breaks it reads what it would rebuild. The set is derived from the recipe instead of
listed in the test, so an argument a later edit adds to a `LABEL` is covered without the test being edited. The same
test asserts each label key is still written, which is the half a moved `ARG` could silently drop.

## D2 — the tag removal stays

The removal costs 0.73 s and the cache it does not touch is what makes the case affordable, so it is not the thing to
optimise. Keeping the tags instead would save about a second and break the case: with a matching image present,
`server deploy`'s step 3 answers `satisfied`, builds nothing, and `case_server_images`'s subject — the build and the
labels it leaves behind — goes unmeasured. The absent state is worth the second; the wrong `ARG` scope was worth
minutes.

## What is not measured

- **A real `agent-image` build after the move, from this tree.** musl.cc's certificate expired on the day of the
  change (its `notAfter` is Nov 26 01:56 UTC, and `curl -fsSL` answers exit 60), so a from-scratch build of the real
  recipe could not finish and the scope claim was measured on a stand-in recipe instead: two variants of a
  `RUN`+`LABEL` Dockerfile over a base image already on this machine, built twice with changed values and
  `--progress=plain` read for `CACHED`. The end-to-end proof is one `scripts/verify.sh` run, whose first image build
  pays one full rebuild — the argument scope is itself a cache key, so a moved `ARG` invalidates the layers it is no
  longer in scope of.
- **The base recipe's move on its own.** That recipe arrived with the image it is (ADR-0114) and was not built after
  the change: its label-only arguments are the agent image's three, in the same shape, and the cost the stand-in
  measured is the mechanism rather than a recipe. What it owes is the same one full rebuild in the next gate run.
- **The 34-to-43-minute figure as a floor.** A from-scratch build still pays every layer, so what this decision
  removes is the rebuild a warm cache should have answered, not the one a new crate set or a new base digest costs.
- **Docker's own behaviour on another builder.** The scope rule is BuildKit's and the `CACHED` lines are the evidence;
  a daemon without the containerd image store was not measured here.

## Consequences

- `AGENTS.md`'s "Build and gates" now says what a rebuild costs and why, instead of blaming the tag removal, and points
  here; `scripts/verify.sh`'s comment above `case_server_images` says the layer cache is what keeps its build cheap, so
  the removal is not the next reader's thing to delete; and `docs/gate.md`'s cost table says the same, with the
  removal's price named as the 0.7 s it is rather than the build's.
- A commit that changes only the version, the git tag or the binary relabels the images and touches no other layer.
- An argument a `RUN` reads stays above that `RUN`, and the cost of changing it is a rebuild of what it feeds — which
  is the behaviour the file's comments describe and the test now distinguishes.
