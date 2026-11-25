# Holosmith

Holosmith runs coding agents in containers on machines you own, and lets you drive them from a browser or phone. One static `holosmith` binary is the server, the hub, the proxies and the deploy tool; `web/` is the app it embeds.

The README is the human overview. This file is what an agent needs before changing code.

## Linear scope

This repository tracks work in exactly one Linear project:

| Field | Value |
|---|---|
| Project | **Holosmith** |
| Project ID | `P-HOL-2` |
| Project UUID | `f3a9d020-848a-437b-baec-eba402b25b15` |
| Team | `Holosmith` (`HOL`) |
| URL | https://linear.app/adamolsson/project/holosmith-761104bbb042 |

Rules:

- Always pass `project: "Holosmith"` when calling `list_issues` or `save_issue`.
- When creating an issue, set the Holosmith project and the `HOL` team explicitly. Never leave either implicit.
- Issues created before the move keep their `PER-*` identifiers and now belong to the same project, so they are in scope; new issues are `HOL-*`.
- Never create, read, update, comment on, or close issues in another project. In particular, `VulkanRenderAPI` belongs to a different repository and is out of scope here — do not list it, do not borrow its issue numbers, and do not treat it as precedent.
- If a Linear call cannot be scoped to Holosmith, report that to the user and stop. Do not fall back to a workspace-wide query.

Issue tracking is remote only. Do not add `HS-*`/`HOL-*`/`PER-*` citations to code, comments, help text, or docs, and do not recreate `docs/stories`, `docs/review`, `docs/notes.md`, `RESUME.md`, `PROMPT.md`, or `HolosmithSpec.md`.

## Repository map

| Path | What lives there |
|---|---|
| `crates/holosmith/src/` | The binary: CLI, HTTP, sessions, events, Docker backend, proxies, deploy/release |
| `crates/holosmith/tests/` | Integration tests; `tests/support/` and `tests/fixtures/` hold shared helpers and real pi recordings |
| `crates/holosmith/migrations/` | Embedded, forward-only SQL migrations |
| `crates/holosmith/images/` | Embedded Dockerfiles for the agent, proxy, builder and smoke images |
| `crates/holosmith/fixtures/` | The recorded turn the smoke image replays |
| `web/` | Vite + TypeScript PWA; built into `web/dist` and embedded in the binary |
| `scripts/` | Build, static-artifact checks, embed checks and the end-to-end gate |
| `README.md` | The human overview, including the getting-started path: install, key, project, phone, tailnet |
| `docs/*.md` | Operator guides: requirements, egress, remote access, hub, keys, troubleshooting, architecture |
| `docs/config.md` | Every config key: type, default, rule, and what reads it |
| `docs/keys.md` | The tool-key runbook: rotating a stored key, why no restart is needed, and the five `doctor --full` lines |
| `docs/gate.md` | What a full verification measures, phase by phase and case by case, and what each piece costs |
| `docs/adr/` | Architecture decisions, one decision per file |
| `docs/schema/previous.sql` | The previous release's schema; `tests/schema_previous.rs` compares it |
| `pins/pi.txt` | The single pinned pi version |
| `GLOSSARY.md` | The project vocabulary — read it before naming anything |

## Build and gates

The workspace is Rust 2024, `rust-version = "1.99"`, one binary crate. `unsafe_code` is denied and
`unwrap_used` is warned; CI treats warnings as errors.

One command proves the tree:

```bash
. ~/.pi/toolchains/env.sh && scripts/verify.sh
```

It takes about half an hour on this host once `target/` is warm, and nearer forty in a fresh worktree
(~5 min 20 s of it the session offline build case). It is 34 to 43 minutes when the image cases rebuild
`agent-image` (two such runs measured here: 34 and 43 minutes), because `case_server_images` removes both
product tags at its start and the rebuild is what follows. Green is its last line: `verify: <n> assertions, ok`.
A `FAIL case_x/assertion` line above it names the case and the assertion; a case that cannot measure its
fact prints `SKIP` with the reason instead of passing quietly. The count moves as cases are added, so read
the shape of that line, not its number. It wants a working Docker daemon for the container cases.

The same gate is three commands of its own, cheapest first, when one of them is all you need:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

A worker runs none of that block. `scripts/gate-quick.sh <test-binary>...` runs `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test --lib`, and only the integration binaries you
name; `--list` prints their names. Name every binary whose module you touched, and if your change names
none, run `--lib-only` and say so in your report. The footer counts the binaries that did not run —
`<n> of <total> test binaries run` — so an under-named change is visible in the run's own last line.
Relinking every test binary costs about a minute and running them about two and a half, which is where
a worker's turn goes when it re-runs `cargo test` two or three times. `cargo test` and `scripts/verify.sh`
belong to the parent: its run is the proof, and a green quick gate says on its last line that the tree
is not proven. The two tiers are therefore recorded here and not in an ADR — ADR-0003 still reads as if
there is one gate, which is the cost of keeping the rule where an agent will read it.
Both gates also exit 3 when the image's vendored crate set came from another `Cargo.lock`
(`scripts/check-crate-set.sh`, which both run as a precondition before their first phase), which is a stale
`agent-image` and an image rebuild rather than a code fix. `scripts/verify.sh` also exits 4 when a case could not
measure because a build could not fetch a source it pins — the case prints `UNREACHABLE <case>` at column 0 and stops
the run (ADR-0111). That one is a host that did not answer and not a red tree: a `FAIL` there would make `holosmith
update` record a gate refusal nobody made, which is why `server deploy`'s image step answers 4 as well.

`scripts/verify.sh` runs those three, then `scripts/build.sh` (which builds the web app and checks what
the artifact ships), `scripts/check-embed.sh`, and a boot of `dist/<host-triple>/holosmith` — the
artifact the `build` phase just produced, never `target/debug`. Do not invent a second build path: the
phases are the build.

`scripts/verify-in-container.sh [<verify.sh argument>...]` runs that same gate **in a container** on this
machine's daemon (ADR-0109): `agent-image:<the Cargo.toml version>` by default, the tree and its git dirs at their
own absolute paths, the invoking uid, `--network host`, and the host's `docker` CLI and socket mounted in for the
run. It parses no argument of its own — every argument and every exit status is `scripts/verify.sh`'s, so `--help`
there is the gate's usage — and it needs no host toolchain, because the image's is the gate's. Its refusals (root,
no `docker` CLI, a `DOCKER_CONTEXT` with no `DOCKER_HOST`, no daemon, no image, no workspace version, a tree the
daemon cannot see) are exit 2 with one sentence naming the fix. A failed run's work dir is under
`target/tmp-verify-in-container`, and the gate's own `kept work dir:` line prints the container path
`/verify-work/tmp.XXXX`, which is that directory's `tmp.XXXX`.

The `.` line is part of the command, not a convenience: `~/.pi/toolchains/env.sh` puts rustc 1.99, both
musl targets, the cross gccs and node/npm on `PATH`. `verify.sh` needs the musl cross toolchains there
**before** it runs, and the default `~/.cargo`/`~/.rustup` toolchain does not have them — its `build`
phase fails with a missing `x86_64-unknown-linux-musl`, which reads like a code error and is not.

Run it detached with the output going to `target/verify.log`, and poll that file for the sentinel; do
not block on the command's own stdout. That one fixed name is deliberate — the poller has to find the
log — and the honesty about the rest of `target/` is worth stating: `$WORK` is a `mktemp -d` and is the
only place a failed run's evidence lives, while every scratch tree a *case* makes is
`target/verify-<case>-$$` and carries the run's pid — except the few that have to be short, the `$WORK` socket
gardens and `case_rollback`'s data root, which a killed run leaves behind under `/tmp`, where the sweep does not
look. So a run that is killed leaves its scratch behind under a pid that is gone, and `target/verify.log` from it
stays where a poller would read it: check the file's mtime against the run you started before trusting a sentinel.

A failed case says where its evidence is: the `work dir kept:` line under the `FAIL` names `$WORK`, the tree
the case built its state in — its config, its responses, its shims, its log, and its socket. Read that tree
before the next run's cleanup removes it.

**A case that binds a unix socket must keep its path short.** `AF_UNIX` caps one at 107 bytes, and a name
built under the checkout is `$repo_root` plus 38 to 56 bytes — `target/verify-<case>-<pid>/docker.sock`,
`…/data/run/drain.sock` for a boot's admin socket, `…/data/smoke/admin/drain.sock` for a smoke run's,
depending on the case's name and the pid's digits — so a case binding there would need `$repo_root` itself
under 51 bytes (107 − 56), and the 83-byte worktree that made one fail built names of 122 to 139. No case
binds a host-to-host socket there any more: every `fake_docker_ping` stand-in is in `$WORK`, a `mktemp -d` of
about 20 bytes, as are the boots' `--admin-dir`s except `case_upgrade`'s and `case_web_update`'s, so no segment
of those names is the checkout's. The exceptions are the sockets whose directory is also a container mount source,
since a mount source has to be a path the daemon can see: `case_smoke`'s smoke root, and `case_upgrade`'s and
`case_web_update`'s data roots because `promote` puts the smoke root at `<data root>/smoke`. Those roots stay
under the checkout; `case_upgrade` and `case_web_update` audit their names with `scripts/verify.sh`'s
`unix_socket_fits` — a checkout too deep for one is a `SKIP` naming the depth rather than a `FAIL` naming a
missing socket, which is why `case_web_update`'s root is `verify-web-` and the pid, eleven bytes before the
digits — and `case_smoke`'s drain socket is the one binder a long path costs nothing, because `serve` warns
rather than refusing and the case measures the turn, not the drain. Every socket a case binds for itself except
that one is audited by the same helper, so a `TMPDIR` deep enough to overrun the cap is a `SKIP` naming the byte
count and the cap: the bind raises in a python child whose stderr the gate discards, and a missing socket is
also what a too-long path looks like.

And never `pgrep -f` a pattern that also appears in the command line you are running: the match is your
own shell, and `verify.sh` boots processes named for the binary anyway, so the name proves nothing
either way.

## Child hygiene: a killed run's children are the next run's problem

`cleanup` is an `EXIT` trap, and SIGKILL runs no trap. A run that is killed — a worker's turn ending, a
tool timeout, `kill -9` — leaves its children reparented to `systemd --user`: a `holosmith serve` that
holds a port, node push receivers, python stand-ins. They do not exit on their own, and the gate never
notices, so they accumulate across runs.

`verify.sh` therefore sweeps *before* its first phase and claims a record for itself (ADR-0089):

- `--claim` writes `target/verify-run-<pid>/owner` (`<pid> <starttime>`) and the run exports the
  directory as `HOLOSMITH_GATE_RUN`, so **every child a case spawns carries the marker with no spawn site
  opting in** — which is what makes the next case someone adds leak-proof too.
- The record is **not** removed when the run ends, not even a green one: the trap cannot know it left no
  survivor (one `extra_pid` slot is shared by several cases, and a case can forget its own child). The
  next run reports it, kills what still carries it, and removes it. That is one directory and one line of
  log per run, and it is what turns "every case behaves" into "the gate converges anyway".
- The sweep kills only processes carrying a **dead** run's marker (element equality on the NUL-separated
  `environ`, so `verify-run-12` never matches `verify-run-123`), and removes `target/verify-*-<pid>` whose
  trailing pid is gone. It never touches a container, a network, a session row, or anything outside a
  worktree's `target/`.
- A pid is evidence in one direction only: a live pid spares both a stale record and a stale directory,
  even when that pid belongs to something else. That is deliberate; the header of
  `scripts/gate-cleanup.sh` says why, and says not to "fix" it.
- A leak from before the marker is cleared once with
  `scripts/gate-cleanup.sh --legacy --dry-run` (read the listing), then `--legacy --yes`. It prints what
  it will kill first, refuses without a terminal or `--yes`, and only touches argv shapes this gate makes.

A cleanup problem warns and never fails the gate: a leftover listener on a pinned port already fails in
the case that wanted it.

A worker's full gate is **one run, at the end of its turn**. A second run answers a question the first is
still answering, and holding one `bash` call open for the whole half hour is the mistake this rule
exists to prevent: the call outlives the worker, and the run ends with nothing said. If the work is cut
short, report the changed files, the gate state you last saw and what remains, rather than begin again.

Run deadlines are not repository config: `timeoutMs` and `checkpointBeforeDeadlineMs` live only in
`~/.pi/agent/extensions/subagent/config.json` — the tracked copy is `.pi/subagents-config.json`, copied
there by hand — and they are **user-scoped**, so they apply to every project on the machine and to the
end-of-turn drain of a `pi -p` run. A new value takes effect in a new Pi session.

One thing this gate teaches the hard way: `doctor`'s `perms` check reads the file `--config` names and
compares it against the 0644 `server deploy` writes, so a fixture that writes a config with a redirect
under the shell's umask (0664) fails a case that has nothing to do with modes — write fixture configs
with `chmod 0644`, as `deploy_config` does.

The web app has its own gate:

```bash
cd web && npm ci && npm test && npm run build
```

## Prerequisites, and what a `SKIP` means

`cargo build` needs rustc only. Everything past a debug run wants these on `PATH`, and a step that failed for
no visible reason is usually one of them missing:

- **rustc 1.99.1** from `~/.pi/toolchains/env.sh`. There is no `rust-toolchain.toml`: the build fetches nothing
  beyond its declared dependencies. The same toolchain in the same version is also inside the **agent image**, with
  this tree's crate set vendored beside it and the web app's packages in its npm cache, so a session can run
  `cargo build --locked`, `cargo fmt`, `cargo clippy`, `cargo test` and `npm ci`/`npm test` with no flag and no
  network. Full verification has two shapes: on the machine (`scripts/verify.sh`) or in the agent image on that
  machine's daemon (`scripts/verify-in-container.sh`, ADR-0109). A *session's* container still carries no Docker
  daemon (ADR-0105, ADR-0106) — the daemon stays outside, and the wrapper is what hands one to a gate for the
  length of a run.
- **`x86_64-linux-musl-gcc` and `aarch64-linux-musl-gcc`** for the shipped targets. SQLite is compiled from
  source (`rusqlite`'s `bundled`), so the C toolchain crosses too.
- **node and npm** for `scripts/build-web.sh`: Vite 7 needs node 20.19+, and `npm test` — node's own runner over
  `web/test/`, importing TypeScript directly — needs **node 22.18 or later** (22.6–22.17 need
  `--experimental-strip-types`). A bare `cargo build` never runs npm.
- **git 2.38 or newer**: the merge check runs `git merge-tree --write-tree`. macOS' `/usr/bin/git` shim reports
  2.20 until the Command Line Tools are installed; the preflight prints the version it found.
- **`openssl`, twice, for different jobs**: in the agent image, because `holosmith proxy key` runs
  `openssl s_client` (the artifact is static and has no TLS stack, ADR-0048) — a missing one is a 502 from the
  proxy, never a silent fallback; and on the host, because web push signs, derives and delivers through four
  `openssl` children per message (ADR-0071), which `doctor --full` reports as `Check::Openssl` rather than
  something `serve` refuses to start over.
- **`install`, `chmod`, `id`, `openssl`** for `server deploy` (ADR-0056): every file action is one child, and the
  programs `tests/deploy.rs` hands a script are `df`, `tailscale` and `id` — with `systemd` pointed at a path it leaves
  absent, because this host has no init to ask. `install`, `chmod` and `openssl` run for real there, since a shim
  cannot show that `install -m 0600` produces a 0600 file. It spawns no `sudo`, and the product escalates nothing: the
  one command a deploy prints for the account — a `usermod` — is never run, and the two `systemctl --user` commands
  step 5 prints are the operator's own. `--root DIR` builds an unprivileged install tree, which is what every gate case
  uses — no test needs `sudo`.
- **tailscale and systemd** as `doctor` questions only: a LAN-only machine and a hand-run `serve` are supported,
  and since ADR-0086 a deploy installs no unit and asks systemd nothing.

Optional, and each one is a `SKIP` with a reason rather than a failure — a machine without it is supported:

- **python3**: the gate cases that read a database or speak a socket by hand (`case_worktree`,
  `case_compaction`, `case_exit_classification`, `case_usage_survives_resume`, the history case,
  `case_schema_newer`, `case_doctor_quick`) and `case_web_update`, which needs `free_port` because a promotion
  polls the port its config names rather than the one a `--port 0` boot reports, plus `scripts/schema-snapshot.sh`,
  which needs stdlib `sqlite3` because the CLI is not assumed to exist. `cargo test` covers the behaviour either
  way: **treat a `SKIP` line as "not measured here", never as a defect in your change.**
- **`$HOLOSMITH_TEST_IMAGE`** (default `pi-agent:local`): a Docker image with `sh`, `git` **and `node`** (the
  network self-test's probe is a node script, ADR-0044) whose default user is not root, for `tests/container.rs`
  and the two mount/reconcile cases. The two install cases run the artifact in it too, as the machine the command hands
  a home of its own (ADR-0108). It is run as the config's `agent_image`, so a development machine may point
  that key at an image it tagged by hand; the two product images are `holosmith-proxy:<version>` and
  `agent-image:<version>` (ADR-0057).
- **`qemu-aarch64-static`** (or `HOLOSMITH_QEMU_AARCH64`) to run the aarch64 artifact here. It is machine-local and
  **no script in this repo downloads anything**.
- **A Docker daemon.** The development sandbox binds its socket because every story is a fact about a real
  `docker run` — that is host root by another name (anything reaching the socket can mount `/`), so it is
  granted deliberately and only when the socket exists. The empty-root run `scripts/check-static.sh` makes is a
  `FROM scratch` container on it (ADR-0108), so a machine without one prints a `SKIP` where it used to measure.

## The offline workflow: what a session can run

The image carries the crate set and the npm cache (ADR-0106), so a session's container — no network, nothing mounted,
`CARGO_HOME` empty — runs the commands the `rustc 1.99.1` bullet above lists, with no flag. `scripts/build.sh` too,
and the gate is what proves it — `case_agent_offline_build` runs it under `--network none` and reads `file -b` on both
musl artifacts inside the container. `--locked` is the flag to keep; anything that would move the lock is a host's work.

**A session's build spends disk, and that number has a key.** Measured in a session's shape (`--network none`, the
invoking uid, a session's `--cpus 2 --memory 4096m`): `cargo build --locked`, `cargo clippy --all-targets` and
`cargo test --no-run` write **9.2 GB** of `target/`, and both musl release builds take it to **9.6 GB** (ADR-0107).
Almost all of it is debug info — a linked test binary here is 111 MB and 18 MB once stripped, because each of the
integration binaries links the whole crate and carries its own copy of its DWARF. `session_disk_soft_gb` measures the
**whole worktree**, 20 by default, so a session that cleans up and builds again deletes `target/` between the two
rather than meeting the watchdog mid-turn; a tree left behind is what a later session pays for.

**`bash -c`, never `bash -lc`.** Measured in `agent-image:0.1.0`: `bash -c 'command -v cargo'` answers
`/usr/local/cargo/bin/cargo`, and `bash -lc` answers nothing, as this host's uid and as a uid the image has no passwd
entry for. `-l` makes bash a login shell, Debian's `/etc/profile` re-sets `PATH` to its own literal list, and the
three prefixes the recipe's `ENV` adds — cargo, both musl.cc cross linkers — are gone with it; an arbitrary uid's
`HOME` is `/` and holds no profile, so that reset is the whole of what `-l` buys a session.

**A new crate is a host act, in three parts.** The vendored crate set is the lock's and nothing else, and measured
under `--network none`, `cargo add rand` answers ``error: the crate `rand` could not be found in registry index.`` — a
sentence about the image, not about a broken registry. A crate the lock *does* name resolves, which is how the two are
told apart. So a dependency the lock has never seen is a manifest and lock edit that a host makes (with its own
`cargo fetch --locked`) plus a rebuild of `agent-image`; `scripts/check-crate-set.sh` is what refuses the old image
with its own sentence instead of letting every phase report the same symptom (the exit-3 rule is in "Build and gates").

**A `SKIP` measures nothing.** In a container `cargo test` prints a `SKIP` for each integration binary that wants a
Docker daemon, and `scripts/build.sh`'s last step prints one for `git_tag` (the case builds in a `git ls-files` copy, so
there is no `.git` to `git describe`), one for the x86_64 empty-root run (this container has no Docker daemon to make
one with) and one for the aarch64 execution (`qemu-aarch64-static`): a green suite there says the tree compiles and its
daemon-free half passes, and the proof is `scripts/verify.sh` on a host (see "Build and gates").

## Running it by hand, and the traps in it

`serve` refuses a machine that cannot run its backend: the preflight checks the daemon, git and the disk before
the listener binds, prints one sentence per failure naming the fix, and exits 1 (ADR-0028). `--backend stub`
skips all of it and so does `role = "hub"`, which is why every gate boot and every phone-only demo uses one of
the two. Facts that cost an hour when you learn them the hard way:

- **A mount source must be a path the daemon can see, and `-v` does not say when it is not.** Docker resolves a
  bind source in the *host's* namespace, so a `mktemp -d` under a tmpfs `/tmp` becomes an empty root-owned
  directory with no error anywhere: the agent gets an empty `/work` and a `.git` that is not the repository.
  Keep scratch repositories under `target/`, as `tests/container.rs` and `case_container_mounts` do.
- **`docker_host`**: unset means `unix:///var/run/docker.sock`; a rootless daemon lives at
  `unix:///run/user/<uid>/docker.sock`; `tcp://` and `ssh://` are the other accepted spellings, and `ssh://` is
  reported *not checked* rather than passed. `permission denied` on the socket means the `docker` group, and the
  preflight's sentence names the group, not `systemctl`.
- **Docker Desktop's published ports are the VM's**: a `-p` port is reachable from that machine and not from a
  phone or the hub, which is why both listeners are the host's own processes.
- **The container appears seconds after the step returns**, and a killed `docker` CLI does not stop its
  container: anything that inspects a session waits for the row to say `idle`, and the next boot's reconcile is
  what makes a leftover boring.
- **A session's files**: the worktree is `<projects_root>/.worktrees/<id>` on branch `agent/<id>`, made with
  `git worktree add` from a commit the repository already has — no fetch, no remote — so a repository with no
  commit cannot be provisioned from. The mounts are the worktree at `/work`, the repository's `.git` at its own
  host path, and `.git/hooks` + `.git/config` read-only over it (ADR-0035). The agent's `pi-home` and its logs
  live under the **data root**, so a Docker session's data root has to be a path the daemon can see too. Every
  start writes `<data>/sessions/<id>/pi-home/gitconfig` and names it with `GIT_CONFIG_GLOBAL`, which is what lets a
  plain `git commit` in the container work: without it the image has no author to stamp and git refuses (ADR-0103).
- **The container is the agent**, and its shape is one value (ADR-0039, `container.rs`): attached (`-i`, never
  `--detach`), `--rm --init --cap-drop ALL --security-opt no-new-privileges`, one `--internal` network per session
  (`agents_<id>`), the five mounts, and the four switches that keep pi off the network (`NO_PROXY=key-proxy,mcp-proxy`,
  `PI_OFFLINE=1`, `PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0`). "Ready" is a protocol fact — a `get_state` command
  and the `response` that repeats its id, under a 30 s bound — and the pi version is asserted against `pins/pi.txt`
  once per process. Every start is `docker rm -f` then `docker run`, serialised per session (ADR-0037).
- **The isolation is measured at every boot** (ADR-0044): a probe from `agent_image` on a temporary internal
  network. `Passed` is a debug line, `Unknown` (no image, no daemon, a probe that did not finish) one warn, and
  `Failed` refuses the boot with the sentence naming what was reached. `hub` and `stub` run neither, and
  `tests/selftest.rs` prints one `SKIP` without a daemon.

## Measuring the credential path (V3–V5)

Three checks no gate may run, because they need **a real provider key**. `case_key_proxy` covers the half that
needs none (a stand-in `openssl` on `PATH`, asserting the bytes the proxy produces), and the by-hand procedure
is below. It needs a daemon and `agent_image`, a git repository under `projects_root`, a data root under
`target/`, and `min_free_gb` low enough for the machine.

- **Never put the key on argv**: `… holosmith key set <provider>` reads it from **stdin**.
- **V3** — create a session, let it reach `idle`, prompt it twice (one prompt that makes it call a tool, one that
  makes it think), and read the transcript back from
  `GET /api/sessions/<id>/events/history?before=<big>&limit=1000`: both `message.delta` kinds, one
  `tool.start`/`tool.end` pair, `usage` on `message.end`. `docker inspect s_<id>` must show no provider key in
  the environment, and the `pi-home` must carry the dummy.
- **V4** — in the same container, `printf '{"type":"get_available_models","id":"m1"}\n' | docker exec -i
  s_<id> pi --mode rpc --no-session` answers the bundled catalog with `PI_OFFLINE=1` and only the dummy key.
- **V5** — `docker logs hs_key` names the session for a real turn (`session=<id> … status=forwarded`). Put a
  competing value where precedence says it wins (`<data>/sessions/<id>/pi-home/auth.json`) and the proxy logs
  `status=bad-key` with `session=-`; remove it and the turn works again. The provider's environment variable is
  the **weakest** source and does not displace the `models.json` key — which is why the seeded `pi-home` carries
  no `auth.json` and the container environment no provider key.

Two failure signatures worth recognising first: a `502 the provider closed the connection before answering`
means the forwarded request declared a `Content-Length` it did not carry, and `401 … session=- … status=bad-key`
for a session that exists means the proxy is reading a `keys.toml` from before the create — a file mount rather
than the directory.

## The stored tool key

The tool table is a second credential (ADR-0098), spent by `hs_mcp` (ADR-0099), and **measured** by `doctor --full`'s
`tool-key` finding (ADR-0101). A rotation is `printf '%s' '<new key>' | holosmith key set linear` and needs no
restart — the proxy loads the store per request and mounts the directory — and this is the end-to-end that proves the
new key is one Linear accepts. It needs a real key from the workspace's security settings, the internet, a daemon and
a session whose `[linear] mcp` is not `off`:

- inside the session's container, `pi mcp list` shows the **`holosmith-linear`** server, and one `list_projects`
  tool call answers;
- `docker logs hs_mcp | tail` shows the call attributed to it — `session=<id> tool=linear host=mcp.linear.app
  status=forwarded`;
- `holosmith doctor --full` on that machine shows `tool-key: pass` for the key, and the same line turns `fail` on a
  key revoked in the workspace's settings.

What the gate measures instead, with **no key and no internet**: `scripts/verify.sh`'s `case_doctor_tool_key` runs the
artifact three times against a stand-in `openssl` on `PATH` and asserts the three findings — `unknown` with no key
stored, `fail` for a `401`, `pass` for a `200` — plus that `--quick` says nothing about it. The two layers under that
are split by what each can see: `doctor`'s own unit tests pin the sentences and the hub arm, `tests/doctor.rs` drives
the report's shape over a store it wrote (one `unknown` per row with no key, one `fail` per row over an unreadable
store, and nothing at all on `--quick`) with no socket, and `mcpproxy`'s and `relay`'s unit tests pin the probe's
bytes and the one `openssl s_client` argument list.

## Fixtures: re-recording one

`tests/fixtures/pi-turn-*.jsonl` are five real turns replayed by `tests/rpc.rs` through the production mapping:
one answer, one that thought first, one that called a tool, one whose tool result carried an image, and one where
a project-local extension asked the client a question (ADR-0040, ADR-0041, ADR-0043). Replaying needs no key, no
daemon and no network — the records are committed. **Refreshing one needs all three**, and four traps:

- One prompt against a container with the operator's own provider credentials, answered with a driver that
  **keeps pi's stdin open until `agent_settled`** — closing it is pi's orderly shutdown, so a
  `docker run -i < prompt.txt` records the system prompt and stops. Drive from a scratch agent directory under
  `target/`, because a mount source the daemon cannot see is an empty directory, not an error.
- **Every fixture's last line must end with a newline**: the reader splits on LF, so a final record without one
  waits for EOF, which a fake agent holding its stdout open never gives.
- The tool fixture needs a prompt that makes the agent call a tool; the image fixture needs a tool result
  carrying an `image` block, which `read` on a PNG in the container's working directory produces; the extension
  fixture needs `.pi/extensions/<name>.ts` calling `ctx.ui.select(...)` from a `turn_start` handler **and** the
  run launched with `--approve`, which is what makes a project's extensions load. Its `notify` after the dialog
  is the record proving the cancel arrived.
- Answering that dialog is `{"type": "extension_ui_response", "id": …, "cancelled": true}` — the same exchange
  production code has.

`tests/rpc.rs`'s `a_fixture_whose_records_drifted_fails_this_guard` counts the record types per fixture, so a
silent refresh is visible where a mapping assertion cannot be.

## Migrations and the schema snapshot

A migration may **add**. `DROP`, `ALTER … DROP COLUMN` and `RENAME` are refused in any file whose slug is not
`contract` or `contract_*` (`schema::contract_violations`), and `tests/schema_previous.rs` compares
`docs/schema/previous.sql` as a subset — every table and column there must still exist. `verify.sh`'s
`migrations` phase is that second half, named so a migration author sees the rule in the output. A new migration
needs no `build.rs` edit: `cargo:rerun-if-changed=migrations` is already there.

**Refreshing the snapshot is a release step.** Start the release you are leaving once with an empty `--data-dir`
(that migrates it), then `scripts/schema-snapshot.sh <data root>/app.db`, and commit the result **with** the
contract migration that needed it — the diff is the record of what that release gave up. The file carries a
provenance line naming the release it came from.

A database from a **newer** release is not a refusal to start: `serve` warns and continues, because that is the
case expand/contract exists to make safe. `doctor --quick` is where the sentence becomes a refusal, which is why
a rollback across a schema-migrating release cannot start its `serve`.

Compaction (ADR-0032) rewrites the delta rows of messages older than 24 hours into one `message.snapshot` each,
so a seq is **strictly increasing with gaps** and the next one is `last_seq + 1`, never `MAX(seq) + 1`.

## The release path: build, smoke, upgrade, rollback

A release artifact is built **in a container**, so that agent-authored build code cannot run on the host:
`holosmith build [--target <triple>] [--out <dir>]` needs Docker and nothing else. The first run builds
`holosmith-builder:<version>` (~6 minutes: two pinned bases, the musl.cc toolchains, the rust targets); the
container gets the source read-only at `/src`, runs the same `scripts/build.sh`, and the artifact comes back with
`holosmith.sha256` beside it. A directory already holding a *different* binary for the same tag is refused. **The
container bounds what build code can reach; it does not judge what it does** — the diff review is the gate.

`holosmith smoke --binary <artifact> --root <dir>` is the only automated evidence that a candidate can serve a
session at all (ADR-0081). Its `pi` is `holosmith container fake-pi` replaying `crates/holosmith/fixtures/
smoke-turn.jsonl`, so **no provider call is made** and the key proxy is never exercised. Exit **0** passed,
**1** a measurement failed — including the smoke image build, so a promotion must refuse it — and **2** could not
run here (no daemon, no `git`, a `--binary` that is missing, not executable, or for another CPU).
`HOLOSMITH_KEEP_SCRATCH=1` keeps the root. **A story that changes the container, git or proxy paths owes this
command a green exit**; a `--backend stub` dev run does not cover those paths. A machine with no network for the
smoke image's base is a **red** smoke test, not a quiet `SKIP`.

`holosmith upgrade <artifact> --acknowledge-review [--root DIR]` promotes: review acknowledged, smoke, the two
product images when their labels say they changed, the drain, a database backup, the release directory, the
`current` symlink, `systemctl --user restart`, and a twenty-second health poll that puts the previous release back by
itself (ADR-0082, ADR-0084). `--acknowledge-review` is not a formality: a promotion without it is refused before
anything is asked of the machine. Every refusal leaves the machine as it found it and says so — **except a refusal
after the drain**, which leaves the machine draining and says the restart is what clears it.

`holosmith rollback [--root DIR] [--to TAG]` is the emergency brake: drain, switch `current` one release back,
restart, health-poll (ADR-0083). "One back" is the order the releases were **written** onto the tree (mtime),
not tag order; `--to` may name a newer release, which is how a mistaken rollback is undone. It has no
`--acknowledge-review`, and every refusal happens **before** the drain so nothing moves. It does not back up,
restore a database, or touch a row or container — the drain stops idle sessions with `server_restart` and the
next boot resumes them.

## Build settings that bite

- **Never export `RUSTFLAGS`.** `.cargo/config.toml` sets the two linkers and `relocation-model=static` per
  target, and cargo silently ignores `target.*.rustflags` when that variable is set — the build would ship a
  static-PIE binary with no error. `scripts/check-static.sh` asserts `statically linked` and an empty
  `readelf -d`, so the mistake fails loudly instead.
- **The git tag** is `HOLOSMITH_GIT_TAG`, else `git describe --tags --always --dirty`, else `unknown`.
  `scripts/build.sh` sets it; a bare `cargo build` does not, and because `build.rs`'s `rerun-if-changed` lines opt
  out of cargo's default rerun, a new tag on an unchanged tree leaves the old value in place: only
  `scripts/build.sh` guarantees a fresh tag.
- **`pins/pi.txt` is the only copy of the pi version.** Do not repeat the number in code, comments, docs or
  tests — `tests/pin_single_source.rs` fails if it appears in a second tracked file.
- The workspace root has one member, so a command from the root needs `-p holosmith`
  (`cargo run -p holosmith -- version`).
- **The fetch-nothing proof**, once per target: `cargo fetch --locked`, then a release build under
  `docker run --network none` (the shape `case_agent_offline_build` runs); `scripts/check-static.sh` also rejects a
  `source = "git+…"` entry in `Cargo.lock`. It covers cargo only: the binary embeds the web app, so a cargo-only build
  of a clone without `web/dist` still succeeds and serves a placeholder — build the app first and with
  `HOLOSMITH_REQUIRE_WEB=1`, which `scripts/build.sh` always does.

## Invariants an agent must not break

- **One binary, three roles.** `serve` runs the role in the config: `hub`, `server`, or `both`. A hub has no Docker and no sessions; it forwards. A server runs containers and owns the session API.
- **The event log is the source of truth for a session's transcript.** One writer commits batches to SQLite (WAL, `synchronous=FULL`), then broadcasts. Do not add a second writer or bypass the transition table in `sessions/state.rs`.
- **All git work happens on the host.** Sessions get a git worktree and an `agent/<id>` branch; the server merges, diffs, snapshots and publishes. A container never owns the repository.
- **Isolation is topology, not configuration.** Per-session internal Docker networks, an egress proxy for allow-listed HTTPS, a key proxy that keeps provider keys out of containers, an MCP proxy that spends a tool's key for the same hosts (joined by the egress proxy's condition, so a tool's host is unreachable with Internet access off), hardened read-only `.git` mounts, `--cap-drop ALL`, no Docker socket in a session.
- **Two listeners.** The control listener serves the API and the app; the preview listener serves only `/host/id/*` on its own origin. Do not merge them.
- **Forward-only migrations.** Add columns and tables; never drop or rename something the previous release reads. `docs/schema/previous.sql` is the snapshot the contract test compares.
- **The web app is embedded.** `build.rs` embeds `web/dist` (or a placeholder when it is absent). A release build must set `HOLOSMITH_REQUIRE_WEB=1`; `scripts/build.sh` does.
- **No browser in the gate.** Browser-only behaviour belongs in `web/test/` (`npm test`), not in `scripts/verify.sh`.

## Working style

- Read the neighbouring module's doc comment before changing it; the comments carry the reasons, not just the types.
- Prefer one spelling for one idea. The glossary records the chosen word and the words to avoid.
- Add a test at the layer that can actually fail: a unit test for a rule, an integration test for a wire shape, a `scripts/verify.sh` case for a shipped artifact or a real daemon.
- When a change alters a decision, add or update an ADR in `docs/adr/`. Decisions are the durable record now that the old story/spec files are gone.
- Keep the CLI help and user-facing sentences free of internal references.
