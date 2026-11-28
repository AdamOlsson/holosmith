# The gate measures the shape a session's build runs in, and a tree's build is its project's

What was true before: the gate compiled **this** tree inside **this** repository's image, twice. `case_agent_toolchain`
took 552 s doing it — `cargo build --locked`, `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test` and
`npm ci`, as the invoking uid with `--network none` — and `case_agent_offline_build` took another 370 s building the
release artifact the same way. Both existed because the product's session image carried a Rust toolchain and a vendored
crate set, and the gate was the place that fact was held: if the image lost its compiler, those cases went red.

ADR-0128 replaced their **subject** with a build in a session's conditions, on a fixture project; ADR-0129 deleted the
pair and named every claim they had held, the ones something else measures and the ones nothing does. What neither
record wrote down is the rule the deletion leaves behind, and it is the sentence a reader needs before wondering why
nothing in the gate compiles anything any more.

## The decision, in a line

The gate measures the **shape** a session's build runs in — the invoking uid, no network, caches that uid reads and
cannot write, and one fast sentence from the image when a dependency is missing — and it measures no particular tree's
build. A tree's build is its **project's** own CI's, in that project's image, and nothing in the product asserts it.

## D1 — the four facts are the shape, and the shape is the product's

The four facts one case measures are the product's because the product decides each of them, for every project's
sessions alike:

- **the invoking uid**, which may have no passwd entry inside the image and whose `HOME` is `/` — `--user` is the data
  root's owner (ADR-0087), reached through `SessionShape::of` so a session and an `image run` name the same one
  (ADR-0130 D2). A build as root passes where the identity a session actually uses fails.
- **no network** — a session's container is on its own `--internal` network with the two proxies as its only routes, so
  a step with a network is a step a session does not have. A registry answers, and the answer is about the machine.
- **caches written where that uid can write** — the image's crate set and npm cache are root's and read-only for the
  session's identity (ADR-0106 D3), and an install that writes into them fails before it installs anything.
- **one fast sentence from the image when a dependency is missing** — a lock naming a package the image does not hold is
  npm's `ENOTCACHED` in about the time a successful install takes, which is a statement about the image; the same run
  with offline off is an `EAI_AGAIN` block, which is a statement about the network.

Each of the four is a fact about the **container a session gets**, not about what a tree contains. `case_session_build`
is their measurement (ADR-0128 D4), and it is about three and a half seconds because its subject image is a fixture
project and its cache is `COPY`'d recorded bytes.

## D2 — a tree's build is its project's own CI, and this tree carries no pipeline

"This repository's own CI" is a named responsibility, and it is worth saying precisely what fills it **today**, because
the tree carries no CI workflow file: there is no `.github/`, no `.gitlab-ci.yml`, nothing of that shape, and this record
does not add one. What exists is a host's own act — a worker's `scripts/gate-quick.sh` turn and a parent's
`scripts/verify.sh` run — one automatic act, a promotion's gate, and the three below:

- **`holosmith image run <project> -- <cmd>`** (ADR-0130): one command in the image the project's sessions run, taking
  its flags from `SessionShape`, with `--network none`, one writable mount at `/work` and the data root's owner as
  `--user`. It builds the tag when it is absent, under the same lock and into the same log a create uses, and exits with
  the command's own status — so `cargo build --locked` and `npm ci` are two steps for a pipeline that has holosmith, and
  the `docker run` line it prints on stderr is what a pipeline that cannot have holosmith carries instead.
- **`scripts/verify-in-container.sh`** (ADR-0109, ADR-0126 D2): the whole of `scripts/verify.sh` inside
  `hs-holosmith:<digest>`, this repository's own project image, as the invoking uid. That is the one thing today that
  puts this tree's source inside that image and builds and tests it there, and a hand is not the only thing that starts
  it: `holosmith update` runs the candidate tree's own copy of this wrapper, with `HOLOSMITH_VERIFY_IMAGE` set to that
  tree's project image, and its green exit is the acknowledgement that verb takes in place of `--acknowledge-review`
  (ADR-0110). Its network is **the host's**, because four sites in the gate dial a container's own address, so it is
  not the offline claim: it proves the source compiles, formats, lints, tests and installs in the project's image, with
  a network available.
- **the release build's host phase**: `scripts/build.sh` and `scripts/check-static.sh` in the gate's `build` phase build
  `dist/<triple>/holosmith` on the host and assert it is statically linked.

A project's CI is that project's to write and to run. The product's part in it is one command whose shape is one value,
and that division is what this record decides; a reader who wants a pipeline file finds one in their own project's tree,
not in this one.

## D3 — the claim that leaves, named

The sentence that leaves the product is: **"this tree builds offline, with `cargo build --locked` and `npm ci`, and no
flag."** It lived in the gate as the two deleted cases, and ADR-0129 listed it under "What owns nothing". It is **this
repository's own CI's** claim now, and nothing in the product asserts it.

What remains nearby is not that claim, and the distinction is the point of this section:

- **`crates/holosmith/tests/images.rs`** asserts the recipe's **shape** as bytes: the vendor instruction and the marker's
  path beside it, the three packages this project's recipe installs for the gate's own container, and the base recipe
  carrying no toolchain and no cache. A file's contents, read with no container — no build.
- **`scripts/check-crate-set.sh`** compares the marker's digest with this tree's `Cargo.lock` before either gate's first
  phase. Its exit 3 says the image is stale and wants a rebuild; it says nothing about whether a build in that image
  works.
- **The image's own configuration** — `[net] offline = true` at `/.cargo/config.toml`, `npm_config_offline=true` in its
  environment — is the recipe making a build offline rather than a measurement of a container that had nothing to reach.
  A setting is not a measurement.
- **The host `build` phase** builds the release artifact and proves its linkage, on the host, in no container.

So a recipe edit that left that sentence false would not turn the gate red. That is the price, and it is the right one:
a case whose subject is this repository is paid for by every machine that runs the product's gate, and the project that
broke it is the only one that learns anything from it — which is the trade ADR-0129 already made and this record names
the owner of.

## D4 — what the deletion does not weaken

Four things, each already in `scripts/verify.sh`, none of them this record's to add:

- **`case_server_images` still builds the images.** It owns the **absent** state: it removes all three tags, lets
  `server deploy` build `holosmith-proxy` and `holosmith-base`, then runs the `ensure` that builds this repository's own
  project image — one real `docker build` of `docker/Dockerfile`, which no Rust test can make — and then asks each
  image the questions that image answers. The **base** answers `pi --version` against the pin, `git`, the boot's
  `node` isolation probe and a uid that is not root, is asked `command -v` for `cargo`, `rustc` and the cross linker
  where an empty answer is the pass — an **absence** and not a version, so nothing here asks the image for a `cargo` it
  holds — and holds none of the four paths a toolchain would leave. The **project image** answers `pi --version`, `git`,
  `dpkg-query` for `python3` and `ripgrep` (the two packages the gate's own `command -v` guards depend on) and the
  invoking uid. The **proxy image** answers its own uid, a refused host and a refused provider path, and a bind list
  carrying nothing of this tree's binary. 236 s of the run `docs/gate.md` records. A recipe that cannot become an image
  still fails the gate.
- **`case_project_image` still proves a project's recipe becomes an image.** It gives a fixture project a
  `docker/Dockerfile`, removes the base, and lets that project's **first session** build `hs-<project>:<digest>` and the
  base under it; the next two sessions find the tag current, `holosmith image build --force` rebuilds it, and the
  per-image lock and the whole log under `<data>/images` are asserted while it happens. 14 s.
- **`case_session_build` still proves the shape a build runs in.** The four facts of D1, over a fixture project carrying
  node and no compiler (ADR-0128), at about 3.6 s.
- **`case_image_run` is the verb's own**, kept separate on purpose so the shape is never proved only through our own
  flags (ADR-0130 D5).

What went is one claim and only one: that **this tree's source compiles in that image**. The images cases measure what
an image **answers** when a container from it is asked; none of them compiles anything, and that difference is the whole
decision.

## D5 — the fixture's subject is carried forward, not reopened

ADR-0128 D6 asked this record to carry its D2 and D3 forward, and not to reopen them. Carried, unchanged:

- **the subject image is a fixture project** — a recipe `FROM holosmith-base:<version>` plus one `COPY` of a recorded
  cache into `/opt/npm-cache` with `npm_config_cache`, `npm_config_offline` and `npm_config_logs_dir` set by the recipe.
  Node and npm only: every session is guaranteed node because pi is a node CLI and the boot's isolation probe is
  `node -e` (ADR-0044, ADR-0114 D1), and a compiler fixture is refused for cost and for re-creating at project-level
  fidelity the thing being deleted (ADR-0128 D2).
- **the cache is recorded once, committed, and `COPY`'d**, written by the image's own npm beside the fixture's lock
  pair, so nothing is fetched at case time and a stale cache is the case failing about its own furniture (ADR-0128 D3).
- **the assertion is the image's own sentence** — npm's `ENOTCACHED`, naming the package and saying the cache is the only
  place it looked — and not a registry's complaint (ADR-0128 D4).

Nothing here widens or narrows that subject. A later record that replaced it with a compiler fixture would be reversing
ADR-0128 **and** this one, and should say so.

The reason the subject is a fixture at all is the same reason the deletion happened, and it is worth one paragraph here
because it is what makes D1 and D3 one decision rather than two: a project owns its recipe and the toolchain that recipe
installs (ADR-0123), this repository is a project like any other whose image `docker/Dockerfile` builds from the
repository root (ADR-0126), and a vendored crate set is the project's own check because Holosmith reads no project's lock
and no project's crate set (ADR-0127). The product cannot assert a fact about a tree it does not read. It can assert the
shape of the container it gives that tree, and that is what is left.

## What supersedes what

This record supersedes **in part** ADR-0105, ADR-0106 and ADR-0114, and each of the three now carries one more note
saying so: the statement stays true of **this repository's own recipe** — `docker/Dockerfile`, `hs-holosmith:<digest>`
(ADR-0126) — and stops being a **product** fact. What supersedes them is not "the image is this project's", which is
ADR-0126's, but "the gate no longer asserts what that image can build in": that half left with the two cases ADR-0129
deletes, and it is this repository's own CI's now.

ADR-0128 and ADR-0129 stand whole. 0128 decided the replacement case and 0129 decided the deletion and named the claims;
this record writes the rule both assumed and reopens none of 0128's D2, D3, D4 or D5, nor 0129's split between what
something measures and what nothing does — it changes only the **owner** of the second list, and the owner is D2 and D3
above.

Two documents name the rule in prose and are cited here rather than rewritten: `docs/gate.md`'s "Where the time actually
is" paragraph and `AGENTS.md`'s matching sentence in "The offline workflow". Only a citation is added to each; neither
document's claim changes.

## What is not measured

- **This tree's build, anywhere in the product.** No gate case and no integration binary compiles it inside the image
  its sessions run. `scripts/verify-in-container.sh` does, in a shape of the gate rather than as a case in it, whether a
  hand or a promotion's gate (`holosmith update`, ADR-0110) starts it.
- **The offline half of that build.** That wrapper is on the host's network, whoever starts it; the image's `net.offline`
  and `npm_config_offline` are settings, and a setting is not a measurement.
- **A CI pipeline.** There is none in this tree, this record adds none, and it does not claim CI runs today. What exists
  is one command whose shape is one value, started by a host's hand or by a promotion.
- **A project's own equivalent of the deleted pair.** Another project that wants "my tree builds in my image with nothing
  to fetch" has ADR-0128's pattern and `holosmith image run <project> -- <cmd>`; nothing here generalises it into the
  product.
- **The fixture's freshness.** The re-record command in the case's own comment is ADR-0128 D3's, and a stale cache is
  that case failing about its furniture.

## Consequences

- The gate is a **product** gate: it measures what every project's sessions get, and a project that breaks its own
  recipe's build learns it from its own CI rather than taking every machine's gate down with it.
- Four of the images group's cases still measure that a recipe becomes an image and that a built image answers what a
  session needs. None measures what a tree compiles to, and none pretends to.
- The images group is **316 s over eight cases** in the run `docs/gate.md` records, with the pair's 552 s and 370 s gone
  (ADR-0128 D5's numbers, not a re-measurement for this decision).
- Reading ADR-0105, ADR-0106 and ADR-0114: each statement about the toolchain, the crate set and the third image now
  names this repository's recipe as its subject. The product's own images are ADR-0114's base half and ADR-0126 D3.
- `docs/gate.md` and `AGENTS.md` cite this record where they already state the rule, so a reader arriving from the
  operator's map reaches the decision without opening the deletion's record first.
- Nothing in the binary, the config, the wire or `scripts/verify.sh` changes: this is the rule the existing cases
  already keep, written down where the two records before it left it unsaid.
