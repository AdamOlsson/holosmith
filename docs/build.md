# Building Holosmith

The only artifacts are static musl binaries for `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl` (ADR-0005).

```bash
. ~/.pi/toolchains/env.sh     # rustc + both musl cross toolchains + qemu on PATH
scripts/build.sh              # web app, both targets, then scripts/check-static.sh
```

`scripts/build.sh` builds the web app first (`scripts/build-web.sh`, ADR-0007) and then
each target, copies each artifact to `dist/<target-triple>/holosmith` (by rename, so an
interrupted build never leaves a truncated file there), prints its SHA-256 and size, and
finishes by asserting that both are plain static binaries that run with no loader and no
userspace dependencies. `--target <triple>` builds one, `--no-check` skips the assertions.
`scripts/check-static.sh` can be re-run on its own; it also refuses an artifact whose
`git_tag` is not the tag of this tree, so a stale `dist/` cannot pass.

## Gates

The gate set for this repository is:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`Cargo.toml` sets `unwrap_used` to `warn`, and a warning cannot fail a build, so
`-D warnings` is what turns it into a gate. `scripts/build.sh` and
`scripts/check-static.sh` are the artifact gate, and `scripts/check-static.sh` is the
assertion `scripts/verify.sh` calls in HS-006 rather than a second way to check the same
thing; until that script exists this is the documented one-command check. HS-006's
`scripts/verify.sh` is also where `scripts/build-web.sh` (before `build.sh`) and
`scripts/check-embed.sh` belong: `check-embed.sh` is the HS-003 proof of the three embed
cases (app present, absent, required), it runs a nested `cargo build` and so cannot be a
`cargo test`, and it is cheap enough to call from the gate.

`cargo test` needs no node and no `web/dist`: without them `build.rs` embeds a placeholder
page that says the web app was not built, and the asset test in
`crates/holosmith/tests/http_contract.rs` prints a `SKIP` line instead of asserting the
built app's files. A green `cargo test` therefore proves the code, never that the app was
built — `scripts/check-embed.sh` and `scripts/build.sh` are what prove that.

The empty-root run proves there is no dynamic loader and no userspace dependency. It is
not a container run: HS-001's "both run in an empty container" clause stays open until
HS-046's verification run on real Docker hosts (ADR-0005).

`scripts/check-static.sh` carries the assertions HS-003 and HS-004 added, about what the
artifact ships rather than about the ELF: the artifact does not contain the placeholder
marker `build.rs` writes when `web/dist` is missing (so a build without the app cannot
ship), the app it serves from the empty root is byte-for-byte this tree's
`web/dist/index.html`, the migration SQL is compiled in (a byte grep of the artifact for
`CREATE TABLE schema_version`, which survives `strip` and LTO), and under the `--data-dir`
it was given it writes `app.db` and its WAL sidecars — and nothing else, before or after the
stop. The marker is read out of
`crates/holosmith/build_support/web_embed.rs` rather than restated here or in the script.

## Browser-facing guards

The control listener refuses a request that is not your browser before any route sees it
(ADR-0010). On every request that is not a health probe the `Host` header must be in
`allowed_hosts`; every non-`GET` request must additionally carry the control origin as its
`Origin`, `Content-Type: application/json`, and `X-Holosmith-Client: 1`; and when `owner` is
set, a request carrying `Tailscale-User-Login` must carry yours. Only `localhost` and
`127.0.0.1` are implicit. A machine reached by its Tailnet name must therefore list that name
in `allowed_hosts`, and a browser that reaches the control listener over IPv6 loopback must
list `[::1]`; both are refused as `bad_host` otherwise. A refusal is the JSON envelope of
ADR-0008 with the code `bad_host`, `bad_origin`, `bad_client` or `bad_owner`, and no response
ever carries a CORS header.

`GET /api/health` is exempt from all four checks — `is_guard_exempt` is the one list — so the
probe works from `systemd`, from `curl` and from the hub's host registration.

The guard assertions are `crates/holosmith/tests/guards.rs` and are deliberately **not** in
`scripts/check-static.sh`: HS-006's `scripts/verify.sh` is where they are lifted into the
one-command check, on the same list as `check-embed.sh`.

## The database and its migrations

`serve` creates the data root and opens `<data_root>/app.db` before the listener binds, so a
data root that cannot be written is a startup failure and not a half-served process
(ADR-0009). The schema is built from numbered SQL files in `crates/holosmith/migrations`,
embedded at compile time with `include_dir!` and applied in one transaction at startup; a
new migration needs no build-script edit beyond the `cargo:rerun-if-changed=migrations` line
`build.rs` already carries, which is what makes a new file reappear in the next build. The
artifact is the authority: `holosmith version` reports `schema_version`, the startup banner
prints `database <path> schema <n>`, and `scripts/check-static.sh` proves both the SQL and
`app.db` are in the shipped binary.

A fresh build of this crate is a fresh test of the embed: `cargo test` runs
`tests/db_migrations.rs` against temporary files, because WAL cannot be observed on an
in-memory database. `cargo test` also needs no writable `/var/lib/holosmith`: every `serve`
test names a data root inside its own temporary directory, because `serve` now creates and
migrates whatever root the config resolves to, and an omitted key resolves to the built-in
default.

## Prerequisites

- **rustc 1.99.1** (the version in `~/.pi/toolchains`). There is no `rust-toolchain.toml`:
  the build fetches nothing beyond its declared dependencies (HS-001).
- **`x86_64-linux-musl-gcc` and `aarch64-linux-musl-gcc`** on `PATH`. SQLite is compiled
  from source (`rusqlite` with the `bundled` feature), so the C toolchain crosses too.
- **node and npm** for the app shell: `scripts/build.sh` runs `scripts/build-web.sh`
  first, which runs `npm ci` from `web/package-lock.json` and then Vite 7's build. Vite 7
  needs node 20.19 or later. Without them `scripts/build.sh` stops before cargo with a
  message naming this file. A bare `cargo build` never runs npm.
- **git** for `scripts/build.sh`. Not required by `cargo build` itself.
- Optional: **`qemu-aarch64-static`** (or point `HOLOSMITH_QEMU_AARCH64` at one) to run
  the aarch64 artifact on an x86_64 machine. It is a machine-local dev tool installed
  outside this repo; no script here downloads it. Without it the check prints one
  `SKIP` line and still passes.
- Optional: **`bwrap`** (bubblewrap) for the empty-root run. Also machine-local; without
  it the check prints one `SKIP` line and still passes.

## Where the target settings live

`.cargo/config.toml` names the two linkers and sets
`rustflags = ["-C", "relocation-model=static"]` per target, which is what makes the
artifacts `ET_EXEC` static binaries instead of static PIE. **Never export `RUSTFLAGS`**:
cargo silently ignores `target.*.rustflags` when that variable is set, so the build would
ship a static-PIE binary with no error. `scripts/check-static.sh` asserts `statically
linked` and an empty `readelf -d` so the mistake fails loudly instead.

## Git tag

`build.rs` reports the tag in this order: `HOLOSMITH_GIT_TAG` if set, otherwise
`git describe --tags --always --dirty`, otherwise `unknown`. `scripts/build.sh` always
sets it, so an artifact never reports the tag of a stale checkout. Builder containers
have no `.git` in the build context, so image builds pass `HOLOSMITH_GIT_TAG` as a build
argument (phase 2); that is why the fallback makes the build succeed instead of failing.
A bare `cargo build` does not set it, and because `build.rs`'s `rerun-if-changed` lines
opt out of cargo's default rerun, a new tag on an unchanged tree leaves the old value in
place: only `scripts/build.sh` guarantees a fresh tag.

## The pinned pi version

It lives in `pins/pi.txt` and nowhere else; the build embeds it, `holosmith version`
prints it and `doctor` will compare the agent image against it. **Do not repeat the
number in this file or anywhere else** — a test (`tests/pin_single_source.rs`) fails if it
appears in a second tracked file.

## Workspace layout

The Cargo workspace root has one member, `crates/holosmith` (ADR-0004), so commands from
the repo root need the package flag: `cargo run -p holosmith -- version`. Build output
goes to `target/`; shipped artifacts to `dist/`; both are git-ignored.

## Reproducibility

Building on this machine is enough for development. Promotion builds run in a pinned
builder container (spec item 10), so `build.rs` and proc macros never run host-side; the
host has to run only the resulting static binary. Until that container exists (phase 2),
`scripts/build.sh` is the documented path and `scripts/check-static.sh` is the evidence
that the artifact is what it claims.

The fetch-nothing claim has a runnable proof, run once per target:

```bash
cargo fetch --locked
bwrap --dev-bind / / --unshare-net --die-with-parent \
  cargo build --release --locked --offline --target <triple>
```

`scripts/check-static.sh` rejects a `source = "git+…"` entry in `Cargo.lock` for the same
reason, so the dependency set cannot start needing the network without a gate failing.

That proof covers cargo only, and it is not enough on its own since HS-003: the binary
embeds the web app, so a cargo-only build of a clone without `web/dist` still succeeds and
serves a placeholder page. Run `scripts/build-web.sh` first (that is what `npm ci` reads
`web/package-lock.json` for, the same category as `cargo fetch --locked`), and build the
artifact with `HOLOSMITH_REQUIRE_WEB=1` set — which `scripts/build.sh` always does, and
which turns a missing app into a build failure instead of a warning. `scripts/build.sh` is
the documented release path for exactly that reason.
