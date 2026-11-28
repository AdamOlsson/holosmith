# The gate

What `scripts/verify.sh` measures, in the order it measures it, and what each piece costs.
[Back to README](../README.md). The gate's rules and tiers are in [AGENTS.md](../AGENTS.md); this page is the map of
what it runs.

The numbers below are wall-clock from one run on the development host (8 cores, warm `target/`, `dist/` and the
product images already built) at commit `d0cc274`, 2026-11-28, whose last line was `verify: 2586 assertions, ok`.
They are one machine's, so read the shape: which item is expensive, and why. A cold worktree with an empty `target/`
adds the compile — the release build alone, from an empty `target/` in this tree, was 2 m 17 s, a second measurement
in this tree and not part of the run above — and a machine whose layer cache cannot answer the image cases' recipes
adds the image build.

## Three tiers, one set of facts

| Tier | Command | Who runs it | What it is |
| --- | --- | --- | --- |
| worker | `scripts/gate-quick.sh <binary>...` | an agent's own turn | `fmt`, `clippy`, `cargo test --lib`, then only the integration binaries it was named |
| proof | `scripts/verify.sh` | the parent, at the end | the whole thing below, including the artifact |

`scripts/verify-in-container.sh` runs the *same* gate inside this repository's **project** image — `hs-holosmith:<digest>`
from `docker/Dockerfile`, the same image a session here runs (ADR-0126) — on the host's daemon; every argument and exit
status is `verify.sh`'s. `verify.sh --no-build` skips the two artifact
phases (the artifact must already be in `dist/`), `--keep` keeps the work dir of a green run.

## Before the first phase

| Item | Cost | What it is |
| --- | --- | --- |
| cleanup + claim (`scripts/gate-cleanup.sh`) | under 1 s | kills what a *killed* run left behind, then writes `target/verify-run-<pid>/owner`, the marker every child of this run carries |
| `scripts/check-crate-set.sh` | under 1 s | is this repository's image vendored from this `Cargo.lock`? **exit 3** and one sentence when it is not — a stale image, not a code fix |

Both run ahead of the phases, and neither is counted as one.

## The phases, cheapest first

| Phase | Command | Cost | What it proves |
| --- | --- | --- | --- |
| `fmt` | `cargo fmt --check` | 1 s | the tree is formatted |
| `clippy` | `cargo clippy --all-targets -- -D warnings` | 29 s | no lint, and warnings are errors |
| `test` | `cargo test`, with `HOLOSMITH_REQUIRE_WEB=1` when `web/dist` is present | 351 s | 766 unit tests and 74 integration binaries, against a real daemon where one is there |
| `migrations` | `cargo test --test schema_previous` | 4 s | the previous release's schema is still a subset of this build's |
| `quick` | `case_gate_quick` | 14 s | the worker gate's contract — its names, refusals, footer — and one real run of it |
| `cleanup` | `case_gate_cleanup` | 6 s | the cleanup script's contract: the marker it kills by, the prefix it does not, the lock, and a registered checkout this host cannot see skipped rather than refused |
| `build` | `scripts/build.sh` | 80 s | `npm test`, the web build, the musl artifact, and `check-static.sh` (statically linked, empty `readelf -d`, plus the empty-root container run) |
| `embed` | `scripts/check-embed.sh` | 45 s | app present, app absent (placeholder), app required and absent (red) |
| `serve` | every case in `serve_cases` (114) against `dist/<host-triple>/holosmith` | 619 s | the shipped artifact serves: the boot, the API, the event log, the container paths, every proxy, the release verbs |

A full green run on this host is therefore **1149 s, about nineteen minutes**: 405 s of source-level phases, 125 s
of artifact phases, and 619 s of `serve` — **over half of the gate is the serve phase** — and more than a third of that
phase is one case, `case_server_images` at 236 s, the one case that builds all three images. After it the phase is one
fact per case, and the longest of them is the sweeper's whole-minute floor (`case_idle_stop`, 66 s) (ADR-0115,
ADR-0126).

The build phase ends with a check that the artifact's `git_tag` is *this tree's*; on a tree with an uncommitted change
it refuses (`git_tag e440b28 is not this tree (e440b28-dirty): re-run scripts/build.sh`). The gate is a fact about a
commit, and that is where it says so.

## The serve phase: one booted artifact, 114 cases

The phase boots the shipped musl artifact on a random port with a temp config (`owner`, `max_live_sessions = 1`,
`backend = "stub"`, `idle_stop_min = 1`, `turn_timeout_min = 1`) and a temp data root, runs `serve_cases` in order, and
stops it on SIGTERM. The boot itself — the banner and the readiness wait — is under a second of the phase here; the
cases are the rest. Every request is a `/dev/tcp` fetch: no curl, no jq. Each case prints `ok <case>` only once it
passed, so the log names the check behind every verdict.

| Group | Cases measured | Cost | What it covers |
| --- | --- | --- | --- |
| boot and shell | 6 | under 1 s | `/api/health`, the schema version in the banner and the DB, both tables, the app shell, the 404 envelope |
| browser-facing guards | 7 | under 1 s | `bad_host`, `bad_origin`, `bad_client`, `bad_owner`, the pass, the loopback spelling, and `health` exempt |
| event log and transcript | 10 | 25 s | deltas coalesced, the stream, tool output, a tool result carrying an image, history, compaction, usage across a resume, the token budget, a newer schema, a snapshot |
| session lifecycle over HTTP | 30 | 98 s | create/202/409/422, idempotence, list, detail, the session screen, a prompt's turn, a timeout that does not fire, bad body, unknown id, abort, steer, the resume refusals, delete and what it keeps, stopped expiry, an idle stop by the sweeper, the drain, the cap's eviction and refusal, exit classification |
| pwa, push and machines | 8 | 34 s | the manifest and the install sentence, one notification per session, `hosts`, forwarding, machine trust, `computers`, a cross-machine read, the hub's deploy |
| git and worktree | 11 | 13 s | a real repository: worktree and its refusals, changes, diffs, the screen, merge and its three strategies and guards, publish, backup, and a delete that keeps the work |
| docker backend | 8 | 35 s | the preflight, the hardened `.git` mounts on a real daemon, an agent launch, a resume, a turn, reconcile and its orphan sweep, the isolation selftest |
| proxies | 7 | 12 s | key, MCP and egress proxies by their bytes (with a stand-in `openssl`), the Internet-access toggle on two real networks, the preview listener |
| doctor and disk | 5 | 22 s | `--quick`, the tool-key finding, `--full`, the disk floor and the disk quota |
| images and builds | 8 | 316 s | the two product images and this repository's own project image, an image build's pinned fetch that did not answer classified as exit 4 rather than as a broken recipe, a project's own image built by the first session that needs it and not rebuilt by the next two, a recipe that does not build leaving its tail on the row and its whole log on disk, the base image rebuilt when a session finds it absent, a build in the session's shape on a fixture project, the verb's own `docker run`, and the smoke run |
| promotion and release | 2 of 4 (`upgrade` and `web_update` skipped here) | 22 s | `upgrade`'s arms, `rollback`'s four arms, a promotion from the app and the update it performs |
| deploy, install, service | 10 | 37 s | `server deploy`, the three install renderings, the root refusal, the unit and the keys, `deploy --ssh`, `status`, `logs`, `uninstall` |

### Where the time actually is

| Case | Cost | Why |
| --- | --- | --- |
| `case_server_images` | 236 s | the one case that empties all three image tags and builds them back: `server deploy` builds `holosmith-proxy` and `holosmith-base` (the generic session image), this case builds this repository's own project image over the base, and then it asks each of the three a dozen container questions. Removing a tag is not what costs this: a removal leaves the layer cache intact, and a build pays only for the steps the cache cannot answer (ADR-0115) — and in this run the cache answered the base's own two `RUN`s and did not answer this repository's project image's, the musl cross-toolchain fetch among them taking about 100 s of the 236 s. A project's image is `FROM` the generic one, so a rebuilt base is every layer of this tree's recipe above it (ADR-0126) |
| `case_idle_stop` | 66 s | `idle_stop_min` is whole minutes, so the sweeper is measured at its one-minute floor rather than behind a knob |
| `case_base_image_lazy` | 45 s | `holosmith-base` removed, then put back by a session in a project with no recipe: the base's own two `RUN`s are network work whenever the cache cannot answer them, and in this run they re-ran — about 11 s of it — and the create that asked for the image is the rest |
| `case_rollback` | 22 s | four arms, each restarting a stand-in and polling health |
| `case_forward` | 17 s | two boots: a hub forwarding to a server |
| `case_disk_quota` | 15 s | the soft limit measured on a real worktree |
| `case_project_image` | 14 s | a project's first session builds `hs-<project>:<digest>`, and the next two find it current |
| `case_server_deploy` | 13 s | the six steps over a `--root` tree, with real `install`, `chmod` and `openssl` children |
| `case_internet_toggle` | 11 s | the Internet-access toggle, on two real networks compared by endpoint count |
| `case_stopped_expiry`, `case_reconcile_orphans` | 10 s each | an aged row swept; containers and networks swept |
| `case_history`, `case_project_image_failure` | 9 s each | a page of a long transcript; a recipe whose `RUN` exits non-zero, leaving its tail on the row and its whole log on disk |

Everything else is at or under 8 s, and **69 of the 112 cases measured here are at or under 1 s** — they are HTTP and
file facts about an artifact that is already running.

The gate no longer compiles this tree in a container, and the rule the arrangement rests on is unchanged: nothing is
fetched at a session's build time — every crate and every npm package comes from the image the session was started from
— because a session runs what its image installed and cached when the image was built, and an allow-list entry is a
host, not a compiler. What a session can build with is therefore its **project's** image's business and not the gate's:
this repository's is `hs-holosmith:<digest>` over `docker/Dockerfile`, and `case_server_images` is where the gate builds
it — from the project's own recipe, by the same `docker build` a session's first start runs (ADR-0126). So what the gate
measures is the **shape** a session's build runs in, on a fixture project, and a project's build inside its own image —
this one's included — is that project's own CI's business (ADR-0131), with `holosmith image run <project> -- <cmd>`
giving that CI the same shape as one command.

The shape has one case, `case_session_build` (ADR-0128): its subject image is a **fixture project**
— a recipe `FROM` the base image plus a `COPY` of a recorded npm cache, so it carries node and no compiler — and its
four facts are the session's shape rather than this tree's toolchain: a build as the invoking uid, `--network none`, a
cache read-only for that uid while npm's log land is `/tmp`, and npm's own sentence for a package the image does not
hold. It costs seconds, because the layer its `COPY` makes is cached and nothing is fetched at case time.

`case_image_run` is the verb's own case, and it is separate on purpose: `holosmith image run <project> -- <cmd>`
runs one command in that same shape, and `case_session_build` may not use the verb it partly exists to justify
(ADR-0130). Its subject is the `docker run` line the verb prints, the exit status it hands back, and the identity and
working directory the line names.

Three of the images group's cases are the **lazy** half (ADR-0124): `case_project_image` makes a project carry a
`docker/Dockerfile` and lets its first session build `hs-<project>:<digest>`, `case_project_image_failure` gives one a
`RUN` that exits non-zero, and `case_base_image_lazy` removes `holosmith-base` and lets a session in a project with no
recipe put it back. A fixture project's build is one `COPY` over the base `case_server_images` just built, so the layer
cache answers it; `case_base_image_lazy` is the other shape, because the base it removes is the one whose `RUN`s fetch
from `apt` and from npm, and those are rebuilt whenever the cache cannot answer them. They are the only cases that
measure a `docker build` this program started *on a session's behalf* rather than as a deploy step.

## What a case needs, and what a SKIP means

A `SKIP` with its reason is a fact about *this machine*, never a defect in the tree: the case measured nothing, and its
`ok` line is withheld, so the run's last line counts them (`SKIP 2 of 114 cases measured nothing`, both of them this
worktree's socket paths, below).

| Needs | Which cases |
| --- | --- |
| nothing beyond the artifact | the boot, guard, event-log, session-lifecycle, git and deploy cases — this is the cheap majority |
| a Docker daemon | the container, isolation, image and build cases, the install cases in their container, and `case_docker_preflight` |
| `$HOLOSMITH_TEST_IMAGE` (default `pi-agent:local`) | the mount and reconcile cases, and the two install cases |
| python3 | the cases that read a database or speak a socket by hand, and `case_web_update` (it needs `free_port`) |
| a network for one pinned base | the two product images and the project image: a host that cannot fetch is red here, not quiet |
| a path short enough for `AF_UNIX` | `case_upgrade` and `case_web_update`, which bind a socket under the checkout and print `SKIP … the socket path is <n> bytes and AF_UNIX caps one at 107` instead |

## Four traps, in the order they bite

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
   inside the default projects root, so a run started from that checkout exits 1 there and stops. This run was a worktree
   under `~/.local/share/holosmith/worktrees/…`, outside the default projects root — so is a checkout whose config
   carries a `projects_root` line — and the case measured 8 s and passed.
4. **A deep checkout path costs `case_upgrade` and `case_web_update`, the two cases that bind a socket under the
   checkout.** This run's path is 132 bytes at `target/verify-upgrade-<pid>/data/run/drain.sock` and 128 at
   `target/verify-web-<pid>/data/run/drain.sock`, both against `AF_UNIX`'s cap of 107, so both cases printed the `SKIP`
   above and the promotion was not measured here.

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

A run stopped by a red case measures only the phases and cases before it, so nothing it prints is a substitute for one
whole green run's tables.
