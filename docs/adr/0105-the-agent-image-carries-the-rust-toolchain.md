# The agent image carries the Rust toolchain, unconditionally

A session's container is Debian bookworm with node, git, openssl, CA certificates and whatever
`agent_toolchains` names — `python3` and `ripgrep` on a fresh machine. It had **no Rust toolchain and no C
compiler**, and Debian's own `rustc` is 1.63 against this tree's `edition = "2024"` and
`rust-version = "1.99"`. Measured in the image:

```text
$ docker run --rm --entrypoint cargo agent-image:0.1.0 fmt --check
docker: Error response from daemon: failed to create task for container: … error during container init:
exec: "cargo": executable file not found in $PATH
```

So the three commands a worker runs — `cargo fmt --check`, `cargo clippy`, `cargo test` — do not exist inside a
session, and the cause is not the egress allow-list: no host would have fixed a missing compiler. The decision is
that the agent image carries the toolchain the tree is written in, as part of the **fixed** set.

## D1 — unconditional, not a variant image

The toolchain goes into `crates/holosmith/images/agent.Dockerfile`, the recipe `server deploy`'s step 3 builds and
every session's container runs. There is no `agent.Dockerfile.rust`, no config key, no second tag and no second label
set: one image, one label comparison, one skip rule.

The cost is the image's size, and it is paid by every machine: **measured 725 MB before, 3.4 GB after** (`docker
images`' disk usage; 333 MB + 2.23 GB of base content). That is the honest price of a session that can build, and it
is paid whether or not a machine's projects are Rust — which is the part a variant image would have avoided.

A variant was refused for the reason one image per role already carries (ADR-0057): the toolchain list is a **label**,
the label is the skip rule, and a second recipe means a second name for the same role, a second arm in
`case_server_images`, and a machine whose `agent_image` key and label disagree about which one a session runs. Two
recipes for one role is the shape that drifts. The egress proxy's allow-list is per-machine config because it is a
policy; this is a compiler.

## D2 — the pin is two `FROM` lines, held together by a test

`agent.Dockerfile` copies its toolchain out of the same rust base `builder.Dockerfile` builds in, and both name it
literally — `rust:1.99.1-bookworm@sha256:…`, the identical reference. It has to be a literal in both files because **a
Dockerfile cannot `include` anything**: no mechanism lets one recipe name another's stage or share a fragment, so a
shared file, a `pins/rust.txt` read by `build.rs` and passed as a build argument, or a base image of our own are all
workarounds for a problem a test solves. A build argument would move the value into the Rust that passes it, which is
the second copy this rule exists to forbid — and it would be a config key, which D1 refused.

What holds the two together is `tests/pin_single_source.rs`: it reads each recipe's `FROM rust:` line, asserts the two
references are the **same string**, and asserts that string's digest appears in exactly those two files and nowhere
else (a third copy in a doc or a review is the stale one). A bump is therefore two edits, and a bump of one is a
failure rather than a session compiled by a different toolchain than a release build.

The musl.cc tarballs are pinned the same way `builder.Dockerfile` pins them: by the SHA-256 in the `RUN` line, checked
with `sha256sum -c` before the extraction. Same two URLs, same two digests, for the same reason — a session compiles
the same tree.

## D3 — `rustup component add` and both musl targets are load-bearing

`rust:1.99.1-bookworm` ships rustup's **minimal** profile. Without `rustup component add rustfmt clippy`, `cargo fmt`
answers `'cargo-fmt' is not installed for the toolchain`, which is a session's first command failing on an image that
looks complete. `rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl` is what makes
`.cargo/config.toml`'s two targets resolve, and `build-essential` is the `cc` the `cc` crate needs for bundled
SQLite — `rusqlite` is `bundled`, so `cargo test` compiles C.

## D4 — `CARGO_HOME=/tmp/cargo`, which the builder deliberately does not set

A session's container runs as the **data root's owner** (`--user <uid>`, ADR-0087), which is not the image's `node`
and has no passwd entry — so `$HOME` is not a place anything can be put, and cargo's default registry location is
unwritable. `CARGO_HOME=/tmp/cargo` is writable by any uid, per-container, and thrown away with the container.

`RUSTUP_HOME` stays at `/usr/local/rustup`: read-only is enough for `cargo` and `rustc`, and every install is a `RUN`
that happens as root before `USER node`. The builder sets neither, because it runs as the operator's uid with a
`$HOME` its entry script creates — the two recipes differ here because the two containers' users differ, and the
comment in each says which.

The gate case used to mount a **warm registry** over `/tmp/cargo` (`$CARGO_HOME`'s `registry/`, 138 MB here): a
session's container has no network, so `Cargo.lock`'s crates have to come from somewhere, and `--offline` made that a
claim rather than a hope. **Amended by ADR-0106:** the crates now come from the image — the recipe vendors the lock's
crate set into `/opt/cargo-vendor` and writes `/.cargo/config.toml` — so a session's build needs no mount and no flag,
and the case measured the developer's registry rather than the product's container for one release too many.

## D5 — the currency rule is unchanged, and that is the trap

A Dockerfile-only edit changes no `org.holosmith.*` **label** name or value: the five labels are still the version,
the git tag, the pinned pi, the binary's digest and the toolchain list. What changes is the **binary**, because the
recipe is `include_str!`ed into it — so `org.holosmith.binary-sha256` differs, `server deploy`'s step 3 rebuilds, and
a machine that upgrades pays the ten minutes. The other edge is worse and worth naming: a build that reuses a cached
tag is exactly what the digest label exists to prevent, which is why the case that measures this reads an image
`case_server_images` built from *this* tree rather than one already on the machine.

`BUILD_TIMEOUT` moved from twenty minutes to forty: the measured build is **10 min 30 s with both bases already
pulled**, and a cold machine also pulls ~2.5 GB of `rust` and `node`. It stays a bound rather than a timeout to fail
by — `Ok(None)` from the build is a sentence, not a hang.

## D6 — what a session still cannot do

It carries no Docker daemon, and that is deliberate: a session with one is a container that can reach the host as
root, which is the isolation this product is. So full verification is not a *session's* work — inside the image, **26
of 68 integration binaries and 23 of `scripts/verify.sh`'s cases measure nothing**, and each prints a `SKIP` naming
the daemon. **Amended by ADR-0109:** a gate whose daemon is handed to it is another matter, and
`scripts/verify-in-container.sh` runs the whole of `scripts/verify.sh` in this image on the host's daemon, so full
verification has two shapes rather than only the host's. `cargo fmt`, `cargo clippy`, `cargo test` and `npm test` are
the subset a session can own, and `scripts/gate-quick.sh`'s `cargo is not on PATH` sentence says both fixes because
two machines reach it: a host needs the line that puts the toolchain on `PATH`, and a session that gets there is
running a broken image.

The version is the same version, in both places: 1.99 from `~/.pi/toolchains/env.sh` on a development machine and
1.99 from the recipe's `FROM` line in the image. A `rust-toolchain.toml` would be a third source and is why the tree
has none.

## Consequences

- `case_agent_toolchain` (in `scripts/verify.sh`, after `case_server_images`) runs the commands in a container
  shaped like a session's — `--network none`, the invoking uid, `--cpus 2 --memory 4096m` and the tree copied under
  `target/` — and reads the pinned version out of the recipe rather than restating it. Measured: fmt 1 s, clippy
  ~50 s, `cargo test --no-run` ~153 s, and one session's `target/` measured **8.9 GB** against
  `session_disk_soft_gb`'s default of 10, so a Rust project has little headroom and a second build's `target/`
  exceeds it. **Amended by ADR-0106:** the registry mount is gone, `cargo build --locked` and `npm ci` are measured
  with no flag at all, and the crates and the npm packages are the image's own. **Amended by ADR-0107:** the default is
  **20**, not 10 — the same tree's `target/` measured 9.2 GB after the build, clippy and `cargo test --no-run`, and
  9.6 GB after both musl release builds — so the shipped cap covers a build tree rather than one compile of it.
- `src/egress.rs`'s loopback assertion accepts either `127.0.0.1` or `::1`: the refusal names the address the
  resolver returned, and inside a container `localhost` resolves `::1` first. The test was host-shaped and failed two
  runs out of two in the image; the other three assertions in it are unchanged.
- Four test sites assumed an external `/bin/kill`, which is `procps` and is not in the `node:…-slim` base: the first
  in-container run failed `serve_cli` and **hung** `db_migrations`, because `Command::new("kill")` needs a file on
  `PATH` while a session sends `kill -TERM …` as a command line and dash answers it as a builtin. They now go through
  `support::signal`, which runs it through a shell and **waits with the kill's own status asserted**, so the same
  breakage is a failure with a sentence rather than a hang. The image gains no `procps`, on the ground that a session
  that needs process tools is its own decision. **Amended by ADR-0109:** the fixed set does gain `procps`, because this
  artifact is itself a subject that shells out to `kill` — `smoke`'s liveness check and its stop — and a shell builtin is
  not a program `Command::new` can find. `support::signal` still goes through a shell, because that is about the command
  line a session sends rather than about a file on `PATH`.
- The agent image grows to ~3.4 GB on every machine (3.53 GB with the crate set and the npm cache in it, ADR-0106).
  **Amended by ADR-0106:** `scripts/build.sh` **is** a gate case — `case_agent_offline_build` runs it, and the
  `scripts/build-web.sh` before it, in `--network none` as the invoking uid with a session's
  `--cpus 2 --memory 4096m`, and asserts that both `dist/<triple>/holosmith` files exist **inside the container** and
  that `file -b` calls each statically linked. What this bullet recorded before that was the same command measured by
  hand with the network on, because it needed one: `scripts/build.sh` built the web app and both artifacts
  (`dist/x86_64-unknown-linux-musl/holosmith`, 8 230 600 bytes, and `dist/aarch64-unknown-linux-musl/holosmith`,
  7 407 832 bytes), so a session can produce a release artifact — and its last step, `scripts/check-static.sh`,
  answered `file: command not found`, so a session could *produce* the artifact and not run that script's own check.
  `file` joins the apt line, so the check runs in the image now, and what stays a `SKIP` there is what is machine-local
  and not the image's: `git_tag` (the case builds in a `git ls-files` copy, so there is no `.git` to `git describe`),
  the x86_64 empty-root run (the image's container has no Docker daemon to build another one on, ADR-0108) and the
  aarch64 execution (`qemu-aarch64-static`).
