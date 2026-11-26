# The gate

What `scripts/verify.sh` measures, in the order it measures it, and what each piece costs.
[Back to README](../README.md). The gate's rules and tiers are in [AGENTS.md](../AGENTS.md); this page is the map of
what it runs.

The numbers below are wall-clock from one run on the development host (8 cores, warm `target/`, both product images
already built) at commit `7d7bf08`, 2026-11-25, less the cost of a case the tree no longer runs. They are one machine's,
so read the shape: which item is expensive, and why. A cold worktree with an empty `target/` adds the compile; a machine
rebuilding `agent-image` adds the image build.

## Three tiers, one set of facts

| Tier | Command | Who runs it | What it is |
| --- | --- | --- | --- |
| worker | `scripts/gate-quick.sh <binary>...` | an agent's own turn | `fmt`, `clippy`, `cargo test --lib`, then only the integration binaries it was named |
| proof | `scripts/verify.sh` | the parent, at the end | the whole thing below, including the artifact |

`scripts/verify-in-container.sh` runs the *same* gate in `agent-image` on the host's daemon; every argument and exit
status is `verify.sh`'s. `verify.sh --no-build` skips the two artifact phases (the artifact must already be in
`dist/`), `--keep` keeps the work dir of a green run.

## Before the first phase

| Item | Cost | What it is |
| --- | --- | --- |
| cleanup + claim (`scripts/gate-cleanup.sh`) | under 1 s | kills what a *killed* run left behind, then writes `target/verify-run-<pid>/owner`, the marker every child of this run carries |
| `scripts/check-crate-set.sh` | under 1 s | is the agent image vendored from this `Cargo.lock`? **exit 3** and one sentence when it is not — a stale image, not a code fix |

Both run ahead of the phases, and neither is counted as one.

## The phases, cheapest first

| Phase | Command | Cost | What it proves |
| --- | --- | --- | --- |
| `fmt` | `cargo fmt --check` | 3 s | the tree is formatted |
| `clippy` | `cargo clippy --all-targets -- -D warnings` | 18 s | no lint, and warnings are errors |
| `test` | `cargo test`, with `HOLOSMITH_REQUIRE_WEB=1` when `web/dist` is present | 430 s warm | 740 unit tests and 73 integration binaries, against a real daemon where one is there; 94 s of the 430 is relinking |
| `migrations` | `cargo test --test schema_previous` | 2 s | the previous release's schema is still a subset of this build's |
| `quick` | `case_gate_quick` | 16 s | the worker gate's contract — its names, refusals, footer — and one real run of it |
| `cleanup` | `case_gate_cleanup` | 7 s | the cleanup script's contract: the marker it kills by, the prefix it does not, the lock, and a registered checkout this host cannot see skipped rather than refused |
| `build` | `scripts/build.sh` | 188 s warm, 317 s after a change | `npm test`, the web build, the musl artifact, and `check-static.sh` (statically linked, empty `readelf -d`, plus the empty-root container run) |
| `embed` | `scripts/check-embed.sh` | 52 s | app present, app absent (placeholder), app required and absent (red) |
| `serve` | every case in `serve_cases` (111) against `dist/<host-triple>/holosmith` | 1774 s | the shipped artifact serves: the boot, the API, the event log, the container paths, every proxy, the release verbs |

A full green run on this host is therefore about **42 minutes**: 476 s of source-level phases, 240 s of artifact
phases, and 1774 s of `serve` — **about three quarters of the gate is the serve phase**, and the serve phase is mostly
container work: the three product images' build, and a full compile of this tree inside them (ADR-0115).

The build phase ends with a check that the artifact's `git_tag` is *this tree's*; on a tree with an uncommitted change
it refuses (`git_tag e440b28 is not this tree (e440b28-dirty): re-run scripts/build.sh`). The gate is a fact about a
commit, and that is where it says so.

## The serve phase: one booted artifact, 111 cases

The phase boots the shipped musl artifact on a random port with a temp config (`owner`, `max_live_sessions = 1`,
`backend = "stub"`, `idle_stop_min = 1`, `turn_timeout_min = 1`) and a temp data root, runs `serve_cases` in order, and
stops it on SIGTERM. The boot itself is about 34 s of the phase (the banner and the readiness wait); the cases are the
rest. Every request is a `/dev/tcp` fetch: no curl, no jq. Each case prints `ok <case>` only once it passed, so the log
names the check behind every verdict.

| Group | Cases measured | Cost | What it covers |
| --- | --- | --- | --- |
| boot and shell | 6 | under 1 s | `/api/health`, the schema version in the banner and the DB, both tables, the app shell, the 404 envelope |
| browser-facing guards | 7 | under 1 s | `bad_host`, `bad_origin`, `bad_client`, `bad_owner`, the pass, the loopback spelling, and `health` exempt |
| event log and transcript | 10 | 25 s | deltas coalesced, the stream, tool output, a tool result carrying an image, history, compaction, usage across a resume, the token budget, a newer schema, a snapshot |
| session lifecycle over HTTP | 30 | 100 s | create/202/409/422, idempotence, list, detail, the session screen, a prompt's turn, a timeout that does not fire, bad body, unknown id, abort, steer, the resume refusals, delete and what it keeps, stopped expiry, an idle stop by the sweeper, the drain, the cap's eviction and refusal, exit classification |
| pwa, push and machines | 8 | 34 s | the manifest and the install sentence, one notification per session, `hosts`, forwarding, machine trust, `computers`, a cross-machine read, the hub's deploy |
| git and worktree | 11 | 12 s | a real repository: worktree and its refusals, changes, diffs, the screen, merge and its three strategies and guards, publish, backup, and a delete that keeps the work |
| docker backend | 8 | 37 s | the preflight, the hardened `.git` mounts on a real daemon, an agent launch, a resume, a turn, reconcile and its orphan sweep, the isolation selftest |
| proxies | 7 | 14 s | key, MCP and egress proxies by their bytes (with a stand-in `openssl`), the Internet-access toggle on two real networks, the preview listener |
| doctor and disk | 5 | 21 s | `--quick`, the tool-key finding, `--full`, the disk floor and the disk quota |
| images and builds | 5 | 1406 s | the three product images, the session's toolchain in a container, an offline build, the smoke run |
| promotion and release | 3 of 4 (`upgrade` skipped here) | 53 s | `upgrade`'s arms, `rollback`'s four arms, a promotion from the app and the update it performs |
| deploy, install, service | 9 of 10 (`server_service` failed here) | 38 s | `server deploy`, the three install renderings, the root refusal, the unit and the keys, `deploy --ssh`, `status`, `logs`, `uninstall` |

### Where the time actually is

| Case | Cost | Why |
| --- | --- | --- |
| `case_agent_toolchain` | 552 s | the tracked tree copied into `agent-image`, then `cargo build --locked`, `clippy`, `test --no-run` and `npm ci`/`npm test` inside it under `--network none` |
| `case_server_images` | 474 s cold | removes all three product tags and rebuilds `holosmith-proxy`, `holosmith-base` and `agent-image`; the removal is not what costs this — a warm layer cache answers that build, and only an argument a `RUN` is in scope of busts it (ADR-0115) |
| `case_agent_offline_build` | 370 s | the same offline build again, reading `file -b` on the musl artifact inside the container |
| `case_idle_stop` | 66 s | `idle_stop_min` is whole minutes, so the sweeper is measured at its one-minute floor rather than behind a knob |
| `case_web_update` | 30 s | a promotion started from the app, polled over its socket |
| `case_server_deploy` | 25 s | the six steps over a `--root` tree, with real `install`, `chmod` and `openssl` children |
| `case_rollback` | 22 s | four arms, each restarting a stand-in and polling health |
| `case_forward` | 17 s | two boots: a hub forwarding to a server |
| `case_disk_quota` | 13 s | the soft limit measured on a real worktree |
| `case_stopped_expiry`, `case_internet_toggle` | 11 s each | a real session network compared by endpoint count; an aged row swept |
| `case_smoke`, `case_reconcile_orphans`, `case_history` | 10 s each | a real smoke run; containers and networks swept; a page of a long transcript |

Everything else is at or under 7 s, and **68 of the 109 cases measured here are at or under 1 s** — they are HTTP and
file facts about an artifact that is already running.

## What a case needs, and what a SKIP means

A `SKIP` with its reason is a fact about *this machine*, never a defect in the tree: the case measured nothing, and its
`ok` line is withheld, so the run's last line counts them (`SKIP 1 of 111 cases measured nothing`).

| Needs | Which cases |
| --- | --- |
| nothing beyond the artifact | the boot, guard, event-log, session-lifecycle, git and deploy cases — this is the cheap majority |
| a Docker daemon | the container, isolation, image and build cases, the install cases in their container, and `case_docker_preflight` |
| `$HOLOSMITH_TEST_IMAGE` (default `pi-agent:local`) | the mount and reconcile cases, and the two install cases |
| python3 | the cases that read a database or speak a socket by hand, and `case_web_update` (it needs `free_port`) |
| a network for one pinned base | the three product images: a host that cannot fetch is red here, not quiet |
| a path short enough for `AF_UNIX` | `case_upgrade` and `case_web_update`, which bind a socket under the checkout and print `SKIP … the socket path is <n> bytes and AF_UNIX caps one at 107` instead |

## Four traps this run hit, in the order they bite

1. **A stale `dist/` makes `cargo test` red before any build phase has rebuilt it.** `tests/rollback.rs`,
   `tests/images.rs` and `tests/upgrade.rs` use the shipped artifact as the release they switch to, and read its
   `version --json`. An artifact built before the newest migration reports schema 7 while the database is at 8, so
   seven rollback tests fail with `reports schema 7 and the database … records 8`. The phase order is `test` then
   `build`, so the repair is `scripts/build.sh` by hand, not a code change.
2. **The build phase is red on a dirty tree** — by design: the artifact's `git_tag` must be this tree's, and a
   `-dirty` describe is not.
3. **`case_server_service` fails on a checkout that lives under the default `projects_root`.** The config pass refuses
   a `data_root` inside `projects_root` ("deleting data must never touch a repository"), and the case puts its data
   root under `target/`. On this machine the repository is at `~/.local/share/holosmith/projects/holosmith`, which *is*
   inside the default projects root, so that case exits 1 in about a second and the run stops there — the 23 cases
   behind it are the tail measured separately, above. A worktree under `~/.local/share/holosmith/worktrees/…` is
   outside the default projects root; so is a checkout whose config carries a `projects_root` line.
4. **A deep checkout path costs `case_upgrade`.** This run's path built a 108-byte socket name against `AF_UNIX`'s cap
   of 107, so the case printed the `SKIP` above instead of measuring the promotion.

## Re-measuring

The gate prints no timings of its own, so time it from outside: prefix every line with the epoch and read the deltas
between `ok case_<name>` and the phase marks. Do not hold the call open for the whole run — start it detached, write to
`target/verify-timed.log`, and poll the file.

```bash
scripts/verify.sh 2>&1 | while IFS= read -r line; do printf '%s %s\n' "$(date +%s)" "$line"; done \
  > target/verify-timed.log
```

Nothing in that block puts a toolchain on `PATH`: a host needs rustc 1.100.0 there before the `build` phase
(`AGENTS.md`'s prerequisites), and the musl cross linker resolves itself — `PATH`, then the tree's `toolchains/`, then
the pinned fetch (ADR-0120).

A run stopped by a red case measures only the phases and cases before it; `case_server_service` above is why this
table's tail was measured separately, and why its tail numbers (76 s for 23 cases) are not a substitute for one whole
green run.
