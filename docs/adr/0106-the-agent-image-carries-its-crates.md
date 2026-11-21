# The agent image carries its crates, vendored from the lock

ADR-0105 gave a session the compiler. It gave it no crates, and a session's container has no network — the only thing
it can reach is the egress proxy — so the first two commands a session runs in this tree fail. Measured in
`agent-image:0.1.0`, `--network none`, nothing mounted over `CARGO_HOME`:

```text
$ docker run --rm --network none -v "$tree:/src" -w /src --entrypoint cargo agent-image:0.1.0 build --locked
error: failed to get `axum` as a dependency of package `holosmith v0.1.0 (/src/crates/holosmith)`

Caused by:
  failed to load source for dependency `axum`
…
  [6] Could not resolve hostname (Could not resolve host: index.crates.io)
```

and the web app's install, the same way:

```text
$ docker run --rm --network none -v "$tree:/src" -w /src/web --entrypoint npm agent-image:0.1.0 ci
npm error code EAI_AGAIN
npm error request to https://registry.npmjs.org/vite/-/vite-7.3.7.tgz failed, reason: getaddrinfo EAI_AGAIN registry.npmjs.org
```

The decision is that the image carries both sets as **files**: `cargo vendor --locked` writes this tree's `Cargo.lock`
dependencies into `/opt/cargo-vendor` (105 crates, 108 MB) and `npm ci` fills `/opt/npm-cache` (12 MB) from
`web/package-lock.json`. What both steps read is the six-file crate set the build context already carries
(`images::AGENT_CONTEXT`), so no second build path and no new context entry was needed.

## D1 — vendored, not a warm registry

The gate's answer until now was a **warm registry**: `case_agent_toolchain` copied `$CARGO_HOME/registry` out of the
host, mounted it at `/tmp/cargo`, and passed `--offline` so the build could not quietly reach the network. That made
the case green and left the product exactly as it was — a session still could not build, because a session gets no
mount and no `--offline`, and the operator's machine had to have built this tree before the case could measure
anything. A test whose premise is the developer's own cache is measuring the developer.

A vendored crate set is the lock's own crates, inside the image, written by the image's own build. It needs no
mount, and the flag it replaces is the point: `cargo build --locked` in a session is the same command as on a host,
with nothing to remember and nothing to mount. `--locked` still refuses a build that would move the lock, which is the
one thing a vendor directory must agree with.

Measured in a `--network none` container with an empty `CARGO_HOME`, the crate set on `/`, and `web/` copied to
`/src/web` for the npm row, as the invoking uid:

| command | time |
|---|---|
| `cargo build --locked` | 49 s |
| `cargo clippy --all-targets` | 38 s |
| `cargo test --no-run` | 1 min 55 s |
| `cargo build --release --locked --target x86_64-unknown-linux-musl` | ~3 min |
| `cargo build --release --locked --target aarch64-unknown-linux-musl` | ~3 min |
| `npm ci` in `web/` | 0.6 s, 1.0 s including the container start |

Those are the measurements the change was decided on; re-measured against the image this recipe builds, on the same
machine: 53 s, 44 s, 1 min 49 s and 2 min 22 s for the x86_64 musl release target.

The vendoring step itself needs the network, at **image-build time** — which the build already has for `apt-get`, the
pi install and the two musl.cc tarballs. So the image build stays a networked step and the container it produces is
the offline one; that asymmetry is the whole shape.

## D2 — the config sits at `/`, and is written after the vendor step

`/opt/cargo-vendor` is only half of a source replacement; the other half is the `[source.crates-io] → directory`
config, and **where** it lives is a decision. Cargo reads `.cargo/config.toml` from the working directory up through
its parents, so a file at the filesystem root is an ancestor of every `/work` a session gets, and of every directory
inside it — one file covers the session, a build from `/work/crates/holosmith`, and a `cargo` run from anywhere else
in the container. The tree's own `.cargo/config.toml`, which names the two musl linkers and `relocation-model=static`,
is deeper and still wins on the keys it sets; the replacement is not one of them, so both halves apply.

The order in the recipe is load-bearing. `/.cargo/config.toml` is written **after** the `RUN` that vendors, because a
`directory` source pointing at a directory that does not exist yet fails with a message about the configuration — not
about the vendor step that has not run — and it is the kind of failure that reads like a broken source replacement
rather than like a missing instruction. `net.offline = true` goes into the same file for the other direction: a build
that asks for something the directory does not hold gets `--offline`'s sentence naming the missing dependency, instead
of waiting on a network that is not there and will not answer.

The tracked file staying source-free is therefore a rule and not an accident of what it happens to hold: a
`[source…]` table added under `/src/.cargo` would win over the root's and the symptom would be an empty vendor
directory rather than a config conflict. `scripts/verify.sh`'s `case_agent_toolchain` asserts it, on the tracked
file's own bytes.

The vendor step's own `CARGO_HOME` is a scratch directory removed **inside the same `RUN`**. Without that the layer
would carry the 108 MB of unpacked crates and a second copy of the same bytes as `.crate` archives; the session's
`CARGO_HOME` stays `/tmp/cargo` (ADR-0105's D4), where cargo writes its own scratch and nothing is fetched.

## D3 — the npm cache is read-only, so the logs directory is not the cache

The npm half is the same decision with a different thing making it awkward: npm's cache is `npm_config_cache`, and the
fill names `/opt/npm-cache` explicitly *and* the `ENV` names the same path, so `npm ci` in a session's `web/` reads it
with no flag. `npm_config_offline=true` is what makes that a claim rather than a preference.

Two order constraints come with it. The fill comes **after** the `npm cache clean --force` that the pi install ends
with, because a clean of a cache that already held these packages is a cache with nothing in it. And the `ENV` block
that says `offline` comes after the fill, because the fill itself needs the network.

The cache is read-only for a session — it is root's, and the session's uid is the data root's owner, an account the
image never created (ADR-0087). npm writes one thing that is not the cache, and its default is `<cache>/_logs`, so
`npm_config_logs_dir=/tmp/npm-logs` is not decoration: without it a read-only cache is an npm that fails before it
installs anything. Both directories are made `a+rX` by the `RUN` that fills them, which is the permission an arbitrary
uid needs and the only one it may have.

## D4 — what a session still cannot do

**Add a crate.** The vendored crate set is the lock's and nothing else, and there is no network to fetch a
crate that is not in it: `cargo add` fails, and so does an edit to `Cargo.toml` that names a dependency the lock has
never seen. That is the boundary, and it is the right one — a new dependency is a lock edit, which is a review and a
host's `cargo fetch --locked`, and a rebuild of this image. A session that needs one is a session whose change has to
reach a machine that can.

The rest of ADR-0105's D6 stands unchanged: no Docker daemon, so the container-shaped integration binaries and
`scripts/verify.sh` stay a host's work.

## D5 — the currency rule is unchanged, and so is the bound

No `org.holosmith.*` label changed: the five are still the version, the git tag, the pinned pi, the binary's digest and
the toolchain list. The recipe is `include_str!`ed into the binary, so **any** edit to it changes
`org.holosmith.binary-sha256`, and `server deploy`'s step 3 rebuilds on a machine that upgrades. The image measures
**3.53 GB** by `docker images`' disk usage — the gate's own `case_server_images` reads it — against the ~3.4 GB
ADR-0105 recorded: about 120 MB of crates and packages, 108 MB vendored and 12 MB the npm cache, plus a few MB of
layers. That is the cost, on every machine, of a session that can build offline. It is the same bargain ADR-0105
struck for the toolchain, and refused for the same reason: one image per role, one label comparison, one skip rule.

`BUILD_TIMEOUT` stays forty minutes. The **10 min 30 s** is ADR-0105's measurement, taken **before** this change with
both bases already pulled; the two steps it adds — a `cargo vendor` of a 105-crate lock and one `npm ci` of the web
app — are **unmeasured** on their own, and the forty minutes is the bound they have to fit in rather than a figure
that includes them.

The **smoke** image is unaffected: it starts from the node base rather than this image, so it inherits neither the
toolchain nor the vendor directory — and it needs neither, because its `pi` is `holosmith container fake-pi` replaying
a recording.

## D6 — the context is six files, and the archive is eight entries

`images::AGENT_CONTEXT` is **six** files, embedded at the path each one has in the repository: the workspace
`Cargo.toml`, the member's, `Cargo.lock`, the member's real `src/lib.rs`, and `web/package.json` with
`web/package-lock.json`. The daemon's context is those six plus the `Dockerfile` and the binary, so the archive it
reads is **eight entries** — `tests/images.rs` asserts the count, the order, and that each embedded text is the file on
disk at its own path.

Six and not the tree. A context is a tar handed to `docker build -` on stdin, so "the tree" would mean copying this
repository into a build of this repository — `target/`, `web/dist`, `node_modules/` and all — where the six files are
the vendor step's **input** and the 108 MB it writes is a layer of the image rather than something a context should
carry. Six is also not a round number: it is exactly what `cargo vendor` needs to resolve this workspace, and the two
manifests are two entries because `COPY` keeps a source's name, so a workspace root's `Cargo.toml` and a member's
cannot travel in one instruction. The member's **real** `src/lib.rs` travels rather than a generated stub because a
target is a source file and not a manifest entry: the three manifests alone answer `no targets specified in the
manifest`, and these six produce, measured, the vendored crate set a vendor over the whole tree does (the entry's own
doc comment carries that measurement). The web pair rides in the same context rather than in a second build path, for
D1's reason: one image, one label comparison, one rebuild rule.

What travels is the entries' **text**, not their paths — each is `include_str!`ed — so editing any of the six changes
the artifact's bytes, and with them `org.holosmith.binary-sha256` and D5's rebuild. A context that read the files where
the deploy runs would need them present there, which is the thing `include_str!` exists to avoid.

## D7 — a stale crate set names itself, in the gate and before a worker's first phase

Under D2's replacement a crate the vendored crate set does not hold fails like this, measured in the image with
`--network none`:

```text
$ cargo add rand
error: the crate `rand` could not be found in registry index.
```

That sentence names the registry and not the image, and `cargo update` after it exits 0 having changed nothing. The
fact that settles it is already in the layer: the recipe writes the digest of the `Cargo.lock` it vendored with to
`/opt/cargo-vendor/.holosmith-cargo-lock`, beside the crates. `scripts/check-crate-set.sh` compares that with this
tree's lock and has three endings, of which only one is not silent: no marker at all — a host fetches its crates and is
not a session — and a matching digest are **exit 0**; a different digest is **exit 3** with one sentence naming the
stale image and the rebuild, in place of the registry-index error.

Two readers, because the question is asked in two places. `scripts/gate-quick.sh` runs the check after the toolchain
check and before the first phase, which is the earliest a worker's first line can carry the answer: a stale image
fails every phase with the same unreadable message, so the reason belongs before them rather than at the end of a cargo
trace. `scripts/verify.sh` measures it twice — `case_agent_toolchain`'s `vendor/lock` assertion reads the marker out of
the image it built and compares it with the tree it copied, and `case_gate_quick`'s seventh arm pins all three shapes
plus the refusal through `gate-quick.sh` itself, because a case that pinned only the failures would stay green while
every correctly imaged session's gate refused.

The exit code and the variable are decisions, not conveniences. 3 is a code neither of that script's other endings can
produce (1 the tree is not proven, 2 the invocation was wrong), so "rebuild the image" is distinguishable from "your
change broke something" without reading the sentence. `HOLOSMITH_CRATE_SET` moves the marker's **path** and nothing
else, because `/opt` is not writable on a host and the gate cannot build an image inside a case — so the mismatch is
measurable with no root and no image, and the sentence is the same wherever the two files are. The path is a literal
with three readers — this script, `scripts/verify.sh`'s `case_agent_toolchain` arm, and `tests/images.rs`'s assertion
that the name is written beside the crates — and its one writer is the recipe's `sha256sum Cargo.lock` line; it is
spelled in each rather than shared through `scripts/gates.sh`, because the Dockerfile's `RUN` would have to carry the
shared definition too.

## D8 — the disk cap is a separate decision, and this one did not move it

`session_disk_soft_gb` is still **10** (ADR-0069), and `docs/config.md`'s row for it is unchanged, because nothing here
changes what a session's worktree holds: the 108 MB of crates and the 12 MB of npm cache are on the daemon's disk, in
the image, shared by every session that runs from it. A session's `target/` is what it was before the crates were
local, and ADR-0105's **8.9 GB** measurement of one Rust project's build stands.

That is a decision and not an omission, because the two facts are now adjacent and easy to conflate: a Rust session
that builds its test binaries once is 1.1 GB from the cap, and a second build's `target/` crosses it, at which point
ADR-0069's watchdog stops the session with `disk_quota` — and no amount of vendoring changes that, because the crates
are read from the image and compiled into the worktree. Moving the default, or exempting a build directory from the
`du`, is its own decision: it is a config default and an amendment to ADR-0069's arithmetic, it wants a number
measured on a project that builds rather than inferred from an image's size, and it has to say what a larger per-session
cap means for `min_free_gb` and for several sessions on one disk. Deferred here, deliberately: this ADR decided what an
image carries, not how much a session may write.

## Consequences

- `scripts/verify.sh`'s `case_agent_toolchain` measures the new fact instead of the old one: no registry mount, no
  `--offline`, `CARGO_HOME` left empty, `cargo build --locked` with no flag, `cargo clippy --all-targets`, the full
  `cargo test`, and `npm ci` in `web/` — all in `--network none` as the invoking uid with a session's
  `--cpus 2 --memory 4096m`. It also compares `/opt/cargo-vendor/.holosmith-cargo-lock` with the tree's own
  `Cargo.lock`, so a vendored crate set from another tree is a failure rather than a build that happens to work. It
  also asserts that the **tracked** `.cargo/config.toml` carries no `[source…]` table, for D2's reason: a nearer one
  shadows the image's replacement, and the failure reads as an empty vendor directory.
- `scripts/verify.sh`'s `case_agent_offline_build` measures the product's path rather than the toolchain's: a
  `git ls-files` copy of this tree at `/src`, `--network none`, the invoking uid, an empty `CARGO_HOME` and a session's
  `--cpus 2 --memory 4096m`, running `scripts/build-web.sh` and then `scripts/build.sh` and asserting that both
  `dist/<triple>/holosmith` files exist **inside the container** and that `file -b` calls each statically linked. No
  mount and no `--offline` is what makes it about the image. It is its own case rather than an arm of the toolchain one
  because a failure should name the release build: ~7 s for the web half and ~5 min 20 s for the two release musl
  targets, in ~550 MB of scratch, measured on this host.
- `tests/images.rs` asserts the recipe's shape without a daemon: that every file in `AGENT_CONTEXT` is the source of a
  `COPY` and every `COPY` source is either in `AGENT_CONTEXT` or the binary, that `file` is in the apt line, that the
  vendor `RUN` precedes the instruction writing `/.cargo/config.toml`, that the config's `directory` is the directory
  the vendor step wrote, that `net.offline` is true, that the vendor's scratch `CARGO_HOME` is removed in the same
  instruction, that the session's `CARGO_HOME` is named by one instruction only, and that `npm_config_cache` names the
  cache the fill writes after the `npm cache clean --force`. Those are the edits a mis-edit breaks five minutes into a
  gate case.
- The first image build on a machine fetches ~120 MB more, and a session's `target/` is unchanged — still the 8.9 GB
  ADR-0105 measured against `session_disk_soft_gb`'s default of 10, which D8 leaves where it is.
- `scripts/check-crate-set.sh` is D7's mechanism and has its own three arms in the gate: absent, stale and matching,
  each run through the real script with `HOLOSMITH_CRATE_SET` pointing at a written marker, plus one more in which
  `scripts/gate-quick.sh` itself refuses with **3**. The matching arm is the one worth pinning — it is what every
  correctly imaged session gets on every run, and a regression that inverted the comparison would leave the other two
  green while every session's gate refused.
- `file` joins the apt line: `scripts/check-static.sh` reads an artifact's link state with `file -b`, and
  `scripts/build.sh` ends with that script, so an image without it builds the tree and then fails the build's own last
  step. The `readelf` the script also runs arrives with `build-essential`.
