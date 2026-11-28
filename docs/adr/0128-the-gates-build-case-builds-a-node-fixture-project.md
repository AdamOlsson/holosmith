# The gate's build case builds a fixture project with node and no compiler

Two cases carry the rule that a session runs what its image installed and cached: `case_agent_toolchain` (552 s) and
`case_agent_offline_build` (370 s). Both are to be deleted, and one small case replaces them: **a build in a session's
conditions**. Its subject is the container shape Holosmith owns — a build as the invoking uid, which may have no passwd
entry inside the image and whose `HOME` is `/`; `--network none`; caches written where that uid can write; and one fast
sentence when a dependency is not in the image — and not this repository's toolchain, which is a fact about this project
and not about a session.

That case needs an image to build in. The image it builds in today is this repository's own project image, and the
question this record answers is what its subject image becomes: a fixture project whose recipe installs a compiler, or
one that carries a toolchain the base already has. **It is the second: a fixture project `FROM` the base image plus a
`COPY` of a recorded npm cache, so it carries node and npm and no compiler, and its missing-dependency assertion is
npm's.**

## The decision, in a line

The case builds a **fixture project** whose recipe is `FROM holosmith-base:<version>` and one `COPY` of an npm cache
into `/opt/npm-cache`, with `npm_config_cache`, `npm_config_offline` and `npm_config_logs_dir` set by the recipe. The
case builds that image itself, then runs a bare `npm ci` in the project as the invoking uid under `--network none` with
a session's limits, and asserts four things: the install resolves from the image and not from a registry; the work tree
it writes belongs to that uid; the cache is read-only for that uid while the only place npm writes is `/tmp`; and a
lock naming a package the image does not hold fails with npm's own `ENOTCACHED` sentence in about the time a successful
install takes.

## Measured before deciding

Everything below is this machine's, on `holosmith-base:0.1.2`, with Docker 29.9.2. The scratch directories were under
this worktree's `target/` and the fixture tag was removed afterwards; nothing was added to the machine's product images.

**What the base actually carries.** `docker run --rm --network none --user 60123:60123 -e HOME=/ holosmith-base:0.1.2
sh -c '…'`, a uid with no passwd entry in the image:

```text
uid=60123 passwd-entry=0 home=/ cwd=/
getent: no passwd entry for 60123
node     /usr/local/bin/node        v24.22.2
npm      /usr/local/bin/npm         11.19.1
npx      /usr/local/bin/npx
git      /usr/bin/git
openssl  /usr/bin/openssl
cc gcc g++ make file python3 python cargo rustc rustup   absent
ls: cannot access '/opt/npm-cache': No such file or directory
drwxrwxrwt 1 root root 4096 /tmp
```

The run cost **0.515 s**. `/opt` holds the node base's own `yarn-v1.22.22` and nothing this product put there. So the
four facts have a toolchain to be measured on with **no install, no pull and no build-time fetch**: node and npm are
there because pi is a node CLI and because the boot's isolation probe is `node -e` run in that image (ADR-0044), which
is the base recipe's own header. What is absent is what the compiler fixture would have had to install.

**What a throwaway fixture build costs.** A two-line recipe (`FROM holosmith-base:0.1.2`, `COPY npm-cache
/opt/npm-cache`) plus the three `ENV`s, context 104 KB, `docker build -t … img/`:

| | Cost |
| --- | --- |
| first build — no layer for the `COPY` yet | **1.373 s** |
| rebuild, nothing moved | **0.653 s**, the `COPY` reports `CACHED` |
| the same rebuild with `-q` | 0.406 s |

The base's own layers are the base's; what the case pays for is one content-addressed `COPY`, which is answered from
the layer cache for as long as the recorded bytes do not move. That is the "built at gate time from a cached layer"
question, answered by measurement: a second or so, once.

**A bare `npm ci` in the session's shape.** The fixture project asks for one package; the cache holds it. As a uid with
no passwd entry, `--network none --cpus 2 --memory 4096m -e HOME=/`:

```text
added 1 package, and audited 2 packages in 298ms
found 0 vulnerabilities
```

**0.656 s** for the whole `docker run`, 298 ms of it npm's. As the invoking uid it is the same, 0.79 s including
docker's own start. `/opt/npm-cache` inside the image is `drwxr-xr-x root root`, so the run **cannot** write it, and
the install still works: the only thing npm writes is its debug log, which the recipe moves to `/tmp/npm-logs`.

**The sentence a missing dependency gets.** A second lock asks for a package the cache does not hold, in the same
shape:

```text
npm error code ENOTCACHED
npm error request to https://registry.npmjs.org/is-odd/-/is-odd-3.0.1.tgz failed: cache mode is 'only-if-cached' but no cached response is available.
npm error A complete log of this run can be found in: /tmp/npm-logs/2026-11-28T00_20_04_755Z-debug-0.log
```

Exit **1**, **0.641 s** for the whole run. Three lines: a code, one sentence that names the package and says the cache
is the only place it looked, and where the log went. That is the assertion, and its quality is what
`npm_config_offline=true` buys — the same run with offline off, under the same `--network none`, is a **network**
complaint rather than a statement about the image:

```text
npm error code EAI_AGAIN
npm error syscall getaddrinfo
npm error errno EAI_AGAIN
npm error request to https://registry.npmjs.org/is-odd/-/is-odd-3.0.1.tgz failed, reason: getaddrinfo EAI_AGAIN registry.npmjs.org
```

**The log land is load-bearing, and measured to be.** With `npm_config_logs_dir` unset — so npm's default
`<cache>/_logs`, which this uid cannot create — the same failure gains two lines about permissions and moves the subject
two lines further away:

```text
npm error Log files were not written due to an error writing to the directory: /opt/npm-cache/_logs
npm error You can rerun the command with `--loglevel=verbose` to see the logs in your terminal
```

**A cache can be recorded once and read cold.** Recording one with the image's own npm 11.19.1 — network on, the
recipe's `npm_config_offline=false` overridden for the recording — produced a **48 KB** directory for the one package,
and reading it back as a uid with no passwd entry, offline, took **0.637 s**. A cache recorded by this host's npm 10.9.9
and read by the image's 11.19.1 also worked, at 104 KB; the image's own npm is the spelling this record recommends, so
the versions agree by construction rather than by luck.

**A trap worth the case author's attention.** The work tree has to be writable by the uid the run uses. A directory
owned by uid 1000 with a run as uid 60123 fails in 0.78 s with

```text
npm error   code: 'EACCES',
npm error   syscall: 'mkdir',
npm error   path: '/work/node_modules'
```

— a second fast sentence, about permissions and not about the image. The case's own scratch directory under
`target/` is owned by the invoking uid, so the plain shape (`--user "$(id -u):$(id -g)"`) needs nothing more; a
deliberately strange uid needs the scratch tree made writable for it first.

## D1 — the fixture is a project, and the case builds it

The subject is a **project's** recipe, which is the file a project's image is built from (ADR-0123), and the case runs
one `docker build` over it. It does not boot a `serve` and create a session in the fixture project to make the program
build `hs-<fixture>:<digest>`. That path is measured twice already — `case_project_image` builds a project's image by
its first session and proves the tag, the lock and the log, and `case_server_images` builds this repository's own recipe
— and what this case is about is the **container** a session's build happens in, which `docker run` answers in under a
second. Booting a server, creating a session and waiting for `idle` would cost minutes to reach the same `docker run`,
and would make the case fail twice over for one cause: once at the build, once at the container.

The recipe the case builds has the base's tag interpolated into its `FROM`, the way `case_project_image` writes its
fixture project's recipe (`printf 'FROM %s\n…' "$base"`). No `ARG` plumbing, no second copy of the tag in a tracked
file, and the recipe's whole text is in the case — beside the reason, which D6 puts there.

## D2 — node and npm only, and the missing-dependency assertion is npm's alone

The four facts are **toolchain-agnostic**: an identity that may have no passwd entry, a network that is not there, a
cache an unprivileged uid reads and cannot write, and a dependency that is not in the image. Node is the one toolchain
every session is guaranteed to have — the base recipe's header says node is there because pi is a node CLI and because
the boot's probe is `node -e` — so the fixture needs **no install, no pull and no toolchain download**, and its only
build-time content is a `COPY`.

So the fixture carries an **npm cache and no cargo cache**, and there is one missing-dependency assertion, not two. A
second assertion against a compiler would need the compiler, which is refused below.

**A compiler fixture is refused for a second reason beyond its cost: it re-creates at project-level fidelity the thing
being deleted.** `case_agent_toolchain` exists because this repository's session image carried a Rust toolchain; a
fixture recipe that installs one is the same image with a different name, kept alive to assert a Rust-shaped fact about
a project that does not exist. It also runs against the direction of two decisions this tree has just taken: a
project's recipe decides its image (ADR-0123, ADR-0126), and a vendored crate set is the project's own check because
Holosmith reads no project's lock and no project's crate set (ADR-0127). A gate that ships a Rust toolchain as test
furniture is Holosmith keeping a project's toolchain in the product after deciding not to.

## D3 — the cache is recorded once, committed, and `COPY`d; nothing is fetched at case time

The recipe is the base plus one instruction:

```text
FROM holosmith-base:<version>
COPY npm-cache /opt/npm-cache
ENV npm_config_cache=/opt/npm-cache \
    npm_config_offline=true \
    npm_config_logs_dir=/tmp/npm-logs
```

and the cache in that context is the bytes the fixture project's lock resolves to — `_cacache`'s content-addressed
store, written by **the image's own npm** with `npm_config_offline=false` for the length of one recording run, and
committed beside the fixture project. It is `COPY`'d rather than filled by a `RUN`, and that is what makes the case
offline at case time: the layer is content-addressed, so the build is `CACHED` until the recorded bytes move, and it
needs no registry, no proxy and no allow-list entry — the one thing the image's `FROM` needs is the base tag, which
`case_server_images` has already built and every session on this machine already has.

The fixture's own source is the pair the cache agrees with: `package.json` and `package-lock.json`, and a second lock
naming a package the cache does not hold. They are **fixture bytes, not a program**: recorded like the pi turns under
`crates/holosmith/tests/fixtures/` are recorded, and refreshable the same way — one documented command, in the case's
own comment, that re-records the cache with the image's npm when the fixture project's lock moves. A stale cache is
then a `npm ci` that says `ENOTCACHED` where the case expected it to install, which is the case failing about its own
furniture rather than about the tree.

## D4 — the assertion is npm's sentence, in a time bound, and not a network complaint

What the case asserts, in the shape D3's recipe gives it:

- **a bare `npm ci` installs from the image** — no flag, no mount over the cache, no `--prefer-offline`, because the
  image's `npm_config_*` environment is the fact (the same discipline `case_agent_toolchain` holds for `cargo build
  --locked` and `npm ci`);
- **the install writes as the run's uid** — `node_modules` in the mounted tree belongs to it, and the cache does not
  move: it is root-owned and read-only for that uid inside the image;
- **a lock naming a package the image does not hold exits non-zero, fast, with `npm error code ENOTCACHED`** and the
  line naming the package — three lines, sub-second (the measurement above), and not the `EAI_AGAIN` block, which is
  what the same run says when `npm_config_offline` is off. The bound is generous — a few seconds — because it is there
  to catch a registry attempt hanging, not to grade npm's speed;
- **and the work tree it wrote is the uid's**, which is the same fact a session has (ADR-0035, ADR-0087) — and why the
  case's scratch tree is under `target/` and not under `$WORK`, a `mktemp -d` the daemon may not be able to see.

A case that ran this in the base image with no cache would measure the missing-dependency fact as an `EAI_AGAIN`
network complaint and get nothing from it; the recorded cache is not convenience, it is what makes the sentence about
the image.

## D5 — what the replacement costs, and the group it lands in

Summed from the measurements: one `docker build` at **1.4 s** cold and **0.65 s** warm, plus two `docker run`s at
about **0.7 s** each — **about 3 s** for the whole case, with nothing fetched at case time and the only build-time
dependency being the base tag, which the machine already has.

`docs/gate.md`'s **images and builds** group is 1406 s today, of which `case_agent_toolchain` is 552 s and
`case_agent_offline_build` is 370 s. Losing both and gaining one case at about 3 s puts that group near **490 s**, and
removes two of the gate's three most expensive entries — a full compile of this tree inside a container, twice —
from the list of what a green run spends its time on. The numbers in that table are a separate decision's to move;
this is the number it will write.

## D6 — where the reason lives, and what the ADR that follows must carry

This record is the decision. **The reason also belongs beside the case**, in the `scripts/verify.sh` doc comment above
`case_*` — that is where every case in that file states what it measures and why its shape is what it is, and where a
reader deciding whether to keep, move or widen the case will look. The reason goes there and not only here — a case
reader should not have to open the ADR to learn why it is shaped as it is — and the case states, in its own comment,
that the subject image is a fixture project carrying node and no compiler, why a compiler is refused (D2), where the
cache comes from and how to re-record it (D3), what the four facts are (D4), and the cost (D5). This ADR is the
argument; the comment is the summary a case reader gets without opening it.

**The ADR that records "the gate measures a build in the session's shape, not a project's build" does not exist yet.**
It is a separate decision, and this record is not it: it must carry D2 and D3 forward — the subject image is a
fixture project `FROM` the base, its toolchain is node and its cache is npm's, and the assertion is the image's own
sentence rather than a registry's — and may not contradict them. A future ADR that widened the case's subject image to
a compiler would be reversing this one, and should say so.

## What this does not prove, and where that is claimed

The gap a node fixture leaves is Rust, cargo, a vendored crate set and `/.cargo/config.toml`, and every part of it is
already claimed somewhere else, by something that can fail:

- **`docker/Dockerfile`** is the vendor step and the `/.cargo/config.toml` replacement — `cargo vendor --locked` into
  `/opt/cargo-vendor` with the lock's digest written in the same instruction, and the source replacement plus
  `net.offline = true` at the filesystem root.
- **`scripts/check-crate-set.sh`** compares that digest with this tree's `Cargo.lock` before either gate's first phase,
  and answers 3 with one sentence when they differ (ADR-0127).
- **`crates/holosmith/tests/images.rs`** asserts the recipe's shape — the vendor instruction, the marker's name, the
  three packages this project's recipe must install for the gate's own container (`file`, `python3`, `ripgrep`), and
  the base recipe carrying no toolchain, no `/opt/cargo-vendor` and no `/opt/npm-cache`.
- **This repository's own offline release build** is measured by running the whole gate in `hs-holosmith:<digest>`,
  which is `scripts/verify-in-container.sh` (ADR-0109, ADR-0126 D2): a cargo build, `fmt --check`, `clippy`, `test` and
  `npm ci` in this repository's own project image, as the invoking uid. That container is on the host's network,
  because `--network none` cannot reach the four sites in that gate that dial a container's own address — so the
  no-network half of this build is not what it measures, and the next paragraph says what does.

**`case_agent_toolchain` and `case_agent_offline_build` are what measure that last line today**: they build this tree
inside this project's image under `--network none`, which is the offline release build the paragraph above names. What
happens to that coverage when they are deleted is **not this decision's**: the decisions that delete them own it, and
this record states the gap rather than closing it.

## Refused

**A compiler fixture.** Build-time network and 60-120 s once, then layer-cached: it is a mini-`agent-image` kept as
test furniture, and D2 gives the second reason. It would prove the four facts on a cargo build, and the four facts do
not need a compiler to be true or to be caught when they stop being true.

**A fixture carrying both caches, with a compiler for the cargo one.** The refusal above wearing the other toolchain's
name: same cost, same direction, one more thing in the gate that no session in the fixture project would run.

**Filling the cache with a `RUN npm ci --cache /opt/npm-cache` at fixture build time.** That is
`docker/Dockerfile`'s shop, and it is right there: a project's recipe may fetch at build time because a build has the
internet. Here it would make the case's image depend on a registry being up on the machine running the gate, which is
exactly what a session does not have and what this case exists to keep out of the measurement — and the fixture would
go stale against its own lock silently, where a `COPY` of recorded bytes fails loudly.

**A `file:` dependency, to avoid a cache altogether.** npm resolves a `file:` spec without a registry and without a
cache, so the install would work offline and the missing-dependency arm would work too — and the "caches written where
that uid can write" fact would not be measured at all, because there would be no cache to read. The cheapest honest
answer is the one whose cache is real and whose absence is npm's sentence.

**Not building an image at all: run the case in the base image.** The base has no `/opt/npm-cache` (measured above), so
every `npm ci` there is the `EAI_AGAIN` block, and the case would assert that a network-less container cannot reach a
registry — true, cheap, and the same fact for every toolchain. The fixture is the layer a project gives itself, which
is the thing the case is actually about.

## What supersedes what

Nothing is superseded. ADR-0126's "What is not measured" lists `case_agent_toolchain` and `case_agent_offline_build`
among the cases that are a real `docker build` of this repository's recipe; this record is about their replacement and
leaves their deletion to the decisions that make it. ADR-0127 D4 is the cargo half of "a missing dependency gets one
fast sentence from the image"; **D3 and D4 here are the npm half**, and the two sentences are different in exactly the
way the two tools are: cargo's names the directory source that replaced the registry, npm's names the cache mode and the
package.

It also adds two sentences to `docs/gate.md`, in the paragraph that already states the rule those two cases hold, so
that the document names what replaces them instead of reading as if the replacement were undecided. Its case tables,
counts and per-case costs are untouched: naming the case in that paragraph's prose is this record's, and moving the
numbers is D5's separate decision.

## What is not measured

- **The case itself.** It exists now — `case_session_build` in `scripts/verify.sh`, with D2, D3, D4 and D5's cost in
  the comment above it (D6) — and the measurements above were made by hand and before it, so they are not its output.
- **A session's real worktree.** Every measurement above is a `docker run` this record made by hand, not a container
  the backend started. The worktree and the identity are the same facts `case_agent_toolchain` measures today, and
  `case_project_image` and `case_container_mounts` are where a session's own mounts are measured.
- **Whether npm's cache format survives a future npm major.** Two versions were measured agreeing — this host's 10.9.9
  and the image's 11.19.1 — which is one data point and not a promise. D3's recording rule (record with the image's
  own npm) is what keeps that from mattering, and a re-record is one command.
- **Whether a machine's layer cache pays for the fixture build after a `docker builder prune`.** The `COPY` layer is
  cheap to rebuild because the base's layers are the base's; that was measured as a first build at 1.4 s, and a
  pruned base is a machine that has to pull the base, which is `case_server_images`' and `case_base_image_lazy`'s
  subject rather than this one's.

## Consequences

- The gate keeps a Rust-free subject for the four facts it measures, and loses two of its three most expensive entries
  when those two cases are deleted.
- The fixture's furniture is recorded bytes — a lock pair and a cache directory — with one documented re-record
  command, and no gate run fetches anything to produce it.
- `holosmith-base` gains nothing: every measurement above ran in the base as it is, and the fixture's only additions are
  a `COPY` and three `env` values.
- A project that wants its own version of this test has the pattern in one place: a recipe `FROM` the base with its
  cache `COPY`'d in, and a build run as the uid the container will run as, with no network.
