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

## Consequences

- `scripts/verify.sh`'s `case_agent_toolchain` measures the new fact instead of the old one: no registry mount, no
  `--offline`, `CARGO_HOME` left empty, `cargo build --locked` with no flag, `cargo clippy --all-targets`, the full
  `cargo test`, and `npm ci` in `web/` — all in `--network none` as the invoking uid with a session's
  `--cpus 2 --memory 4096m`. It also compares `/opt/cargo-vendor/.holosmith-cargo-lock` with the tree's own
  `Cargo.lock`, so a vendor set from another tree is a failure rather than a build that happens to work.
- `tests/images.rs` asserts the recipe's shape without a daemon: that every file in `AGENT_CONTEXT` is the source of a
  `COPY` and every `COPY` source is either in `AGENT_CONTEXT` or the binary, that `file` is in the apt line, that the
  vendor `RUN` precedes the instruction writing `/.cargo/config.toml`, that the config's `directory` is the directory
  the vendor step wrote, that `net.offline` is true, that the vendor's scratch `CARGO_HOME` is removed in the same
  instruction, that the session's `CARGO_HOME` is named by one instruction only, and that `npm_config_cache` names the
  cache the fill writes after the `npm cache clean --force`. Those are the edits a mis-edit breaks five minutes into a
  gate case.
- The first image build on a machine fetches ~120 MB more, and a session's `target/` is unchanged — still the 8.9 GB
  ADR-0105 measured against `session_disk_soft_gb`'s default of 10.
- `file` joins the apt line: `scripts/check-static.sh` reads an artifact's link state with `file -b`, and
  `scripts/build.sh` ends with that script, so an image without it builds the tree and then fails the build's own last
  step. The `readelf` the script also runs arrives with `build-essential`.
