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
`scripts/check-static.sh` are the artifact gate, and `scripts/build.sh` runs
`check-static.sh` itself, so `scripts/verify.sh` calls `build.sh` once instead of checking
the artifact a second way. `scripts/verify.sh` also calls `scripts/check-embed.sh`, the
HS-003 proof of the three embed cases (app present, absent, required): it runs a nested
`cargo build` and so cannot be a `cargo test`, and it is cheap enough to call from the gate.

`scripts/verify.sh` is one command that runs all of it: the three commands above, the
artifact build and its checks, and the boot described under **One-command verification** below.

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

## One-command verification

```bash
. ~/.pi/toolchains/env.sh
scripts/verify.sh
```

Six phases, cheapest first, so a formatting slip cannot cost a two-minute musl build:

| Phase | Command |
|---|---|
| `fmt` | `cargo fmt --check` |
| `clippy` | `cargo clippy --all-targets -- -D warnings` |
| `test` | `cargo test` |
| `build` | `scripts/build.sh` (which runs `build-web.sh` first and `check-static.sh` last) |
| `embed` | `scripts/check-embed.sh` |
| `serve` | the boot below and its cases |

`cargo test` proves the code and never that the app was built, so the artifact phases come
after it and before the boot: the binary is built (phase 4) before it is served (phase 6).
`scripts/build.sh` is called once, because it already runs `scripts/build-web.sh`; calling
both would run `npm ci` twice.

`serve` boots the shipped artifact, `dist/<host-triple>/holosmith` with the triple taken from
`uname -m` (`x86_64` and `aarch64` as ADR-0005 names them), not `target/debug`: this is the
thing that ships, it needs no loader, and it is the artifact `scripts/check-static.sh` has
already proved. A debug binary would embed whatever `web/dist` happened to exist when cargo
last ran, which is a different question. An artifact that is not there fails with a message
naming the path and `scripts/build.sh`.

The boot is `serve --config <work>/config.toml --port 0 --data-dir <work>/data`. `--port 0`
asks the kernel for the port and is also what makes `serve` re-derive `control_origin` from
the bound address (ADR-0007), so the case for a request that passes the guards sends exactly
the origin the guard compares; `control_listen` and `control_origin` are both read from the
startup banner, never assumed. The data root is inside the work dir, so a run never touches
`/var/lib/holosmith` (ADR-0009), and the config file carries `owner`, `max_live_sessions = 1`
and `backend = "stub"`: the `bad_owner` case, the create cases and provisioning are the only
reasons that file exists. The backend comes from the *file* rather than from a `--backend` flag
on purpose — that is half of HS-011's "selectable by config and by `--backend`", and
`case_backend_from_config` is what proves it; `tests/serve_cli.rs` covers the flag, the override
and the default. Every request is a `/dev/tcp` fetch (`fetch` for a body-less request,
`fetch_body` when a body is needed, `fetch_stream` for a response that never ends), so the
harness needs neither `curl` nor `jq`. `fetch_stream` exists because `fetch` reads until EOF:
an event stream sends its header block at once and then nothing until the first event, so
`fetch` would sit in its ten-second deadline and report a working stream as a broken one.
`fetch_stream` reads the header block line by line under bash's own three-second timeout and
stops at the blank line, so an early close, a `404` or a `text/plain` refusal is still saved
and asserted like any other answer. `fetch_sse_until` goes one step further and keeps reading body
lines until one contains a needle — an event stream never ends, so "read until the event that
settles the question" is the only bounded way to watch a producer.

A `serve_cases` array names the `case_*` functions, one per answer a browser gets: the health
payload, the schema version the banner reports against `version --json`, the app shell, the page
that holds the session list and the new-session form, byte-for-byte against
`web/dist/index.html`, the `404` envelope of ADR-0008, one case per guard refusal (`bad_host`,
`bad_origin`, `bad_client`, `bad_owner`), the request that passes all four, the exempt health
probe, the create endpoint (`case_create_session`, `case_create_is_idempotent`,
`case_create_bad_project`, `case_create_cap_reached`), the sessions list (`case_sessions_list`,
which asserts `200`, `application/json`, the session `case_create_session` made and a
`network_default` field the create form's checkbox starts from), the session's event stream
(`case_events_stream`, which reads the stream's header block with `fetch_stream` and asserts
`200`, `text/event-stream` and `no-cache` on the session `case_create_session` made), the backend
the boot chose (`case_backend_from_config`), the provisioning that create starts
(`case_provisioning_steps`, which reads that session's stream until the event says `idle` and
asserts the four step words arrived in order), and the state machine's payload
(`case_state_transition_payload`, which asserts the four step reports claim no state change and
the fifth is exactly `{"from":"starting","to":"idle","step":null,"reason":null}`), one
session by id (`case_session_detail`, which asserts the row the create made, that it carries no
`events` key, and that an unknown id is the `not_found` envelope), the session screen's path
(`case_session_screen`, which asserts `/sessions/<id>` answers the shell byte-for-byte), and the
prompt (`case_prompt_turn`, which asserts the 202 answers the row `running` and then reads that
session's stream until the turn settles, asserting the turn's six kinds in order and the
`idle → running → idle` transitions; `case_prompt_bad_body` and `case_prompt_unknown_session`,
which assert the `422 bad_body` and `404 not_found` envelopes), and the abort
(`case_abort`, which prompts, aborts, and reads that session's stream until the event carries the
abort's own reason, asserting the 202 answered the row still `running` and that the turn ended
`running → idle` with reason `abort_completed`; `case_abort_on_an_idle_session`, which asserts the
`409 session_idle` envelope), and the steer (`case_steer`, which prompts, steers, asserts the 202
answered the row still `running`, reads the stream for the `turn.steer` frame carrying the guidance,
and then polls the row until it says `idle`, which is what proves the turn continued;
`case_steer_on_an_idle_session`, which asserts the `409 session_idle` envelope), and the resume's two
refusals (`case_resume_on_an_idle_session` and `case_resume_unknown_session`; the round trip itself
cannot be driven here because nothing in the API produces `stopped` yet — `tests/resume.rs` proves it
against a row the transition table stopped). A case that must assert one code keeps every higher guard
check correct, or it asserts another guard's code under its own name.

The CORS sweep is deliberately **not** in that array: it runs once after every case and checks
that no answer the run saved carries an `access-control-*` header or `vary: origin`. Being
outside the loop is what makes it cover a case a later story appends, without anyone having to
remember to place the new case before it.

**Every later story adds at least one `case_*` function and one line in `serve_cases`.** That
is what makes this a growing gate rather than a snapshot: a story that adds a route or a
behaviour has one obvious place to prove the shipped binary does it.

`--no-build` skips `build` and `embed`, for a loop on the cases against an artifact already
in `dist/`; it does not skip `fmt`, `clippy`, `test` or `serve`. `--keep` keeps the work dir
of a successful run, and `-h` prints the script's own header comment.

The failure contract: a failure prints `FAIL <assertion>` (`FAIL build/failed`,
`FAIL case_guard_bad_origin/code`) and keeps the work dir, printing `work dir kept: <path>`. For
a failure in the `serve` phase that dir holds `config.toml`, `serve.out`, `serve.err` and one
`.http` and `.body` file per answer; a failure in an earlier phase keeps an otherwise empty dir,
because nothing has been written into it yet. A successful run removes it unless `--keep`, and
ends with `verify: <n> assertions, ok`. Teardown sends `SIGTERM`, waits, and asserts the artifact
exited 0 (ADR-0007), which `scripts/check-static.sh` cannot do because `bwrap` reports the
signal's own status instead.

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

The guard assertions are `crates/holosmith/tests/guards.rs`, and `scripts/verify.sh` lifts
them into the one-command check: the `case_guard_*` functions boot the shipped artifact and
require `bad_host`, `bad_origin`, `bad_client` and `bad_owner` from the requests that must be
refused, `404 not_found` from the request that passes all four, `200` from the exempt health
probe, and no CORS header on any answer. They are still not in `scripts/check-static.sh`,
which proves what the artifact ships rather than how it answers. `tests/guards.rs` remains
the unit-level proof, and the place the cases one boot cannot reach live: repeated headers,
`Origin: null`, each `X-Holosmith-Client` value, and a handler that tries to add a CORS
header.

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
  first, which runs `npm ci` from `web/package-lock.json`, then `npm test` and then Vite 7's
  build. Vite 7 needs node 20.19 or later; `npm test` is node's own runner over the
  transcript's rules in `web/test/` (ADR-0018), and it imports TypeScript directly, so it
  needs native type stripping: **node 22.18 or later** (22.6 through 22.17 need
  `--experimental-strip-types`, and 23.6+ is fine). Without node and npm `scripts/build.sh`
  stops before cargo with a message naming this file. A bare `cargo build` never runs npm.
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
