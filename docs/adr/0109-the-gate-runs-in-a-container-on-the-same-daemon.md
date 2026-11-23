# The gate runs in a container, on the same daemon

The full verification was a host's work: it boots containers through the machine's Docker daemon and then dials them
on their own Docker networks, so it wanted a daemon, a socket, and a process outside every container. It is also a
container's work now: `scripts/verify-in-container.sh` runs `scripts/verify.sh` in the agent image, on that same
daemon, with the tree and its git dirs mounted at their own absolute paths.

There are therefore two supported shapes, and neither replaces the other:

- **on the machine** — `scripts/verify.sh`, unchanged, and still the gate of record (ADR-0003);
- **in a container on that machine** — `scripts/verify-in-container.sh [<scripts/verify.sh argument>...]`, which
  parses no argument of its own and forwards everything, and whose exit status is the gate's.

The daemon stays **outside** both. A session's container still carries none, and that is not in question here
(ADR-0105, ADR-0106): a session with the socket is a container that can reach the host as root. The wrapper is the
one thing that hands a daemon to a gate, for the length of one run, and it does it the way the operator's own shell
already has it — the socket, the CLI and the CLI's plugins, mounted read-only.

## D1 — the flags, each of which is the only shape that measures

Measured on this host (Docker 29.9.2, Ubuntu 24.04), in a probe container on `agent-image:0.1.0`:

| shape | dial `agents_<id>` (internal) | dial a normal bridge | internet |
| -- | -- | -- | -- |
| `--network none` | `Network unreachable` | unreachable | no |
| `--network bridge` | timeout | `fetch failed` | yes |
| `--network host` | `HTTP/1.1 200 OK` | 200 | yes |

- **`--network host`.** Four sites dial a container's own address on a Docker network: `case_preview`'s forward,
  `case_forward`'s container-up check, and `case_server_images`' two proxy checks. Under `--network none` the first of
  them answers `Network unreachable`; on a bridge they answer `fetch failed`, because Docker drops
  container-to-container traffic between bridges. The host's network namespace is the only shape in which those four
  measure at all — which is also why a later "no internet" rule is a host's, not a flag here.
- **`--group-add <the socket's gid>`.** Without it every `docker` call in the container is `permission denied while
  trying to connect to the docker API`, and the gate is fourteen docker-socket cases, twenty-seven `command -v docker`
  guards and the 26 integration binaries ADR-0105 counts, which would all answer it. The group is read from the socket
  (`stat -c %g`) rather than named, because the socket's mode is what decides it, and it differs between a root daemon
  and a rootless one. That flag hands over a **gid**, and a gid is a number; the next bullet is the name
  `server deploy` reads from it.
- **the machine's `/etc/group`, at its own path, read-only.** The gid `--group-add` hands the container's process is
  `984` here — the operator's `docker` group — and an image whose group file has no entry for it answers `id -nG` with
  `node 984` and `id: cannot find name for group ID 984` on stderr, exiting **1**. That is not a cosmetic difference:
  `server deploy`'s step 2 asks `id -nG` whether the account may open the daemon's socket (`deploy::read_account`),
  `ask()` turns a non-zero status into `None`, so the step fails with `named no groups` and the run stops there —
  measured in a full in-container run as `FAIL case_server_deploy/partial/step2`, on a machine whose host run
  is green because the same call there names `docker` and exits 0. `Account::may_open` compares against the constant
  `DOCKER_GROUP = "docker"`, so the **name** is the fact the step needs and the mounted file is what supplies it:
  measured in a probe container with this wrapper's argv, `id -nG` answers `adamolsson docker` with exit 0, and the
  same deploy's step 2 answers `changed: 7 actions; the account node is in the docker group` instead of failing. It
  is a mount rather than a recipe change because no recipe can add a host gid's name: the gid is the daemon socket's
  and differs per machine, while a `groupadd` in the image would have to know it before the wrapper reads it.

  **Rejected: mounting `/etc/passwd` too, measured.** An earlier shape of this bullet mounted both identity files — the
  account's name is what the one printed `usermod` spells (`Account::command`) — and it was measured to cost more than
  it buys.
  `/etc/passwd` is the file `identity::home` reads ("the password entry of the uid, never `$HOME`", `src/identity.rs`),
  so mounting it moves `HOME` **and** every default path from the image's `/home/node` to this operator's
  `/home/adamolsson`, a directory docker creates root-owned here as part of the tree's own mount path. Measured in the
  probe container: `HOME=/home/adamolsson`, `id -un` `adamolsson`, `id -nG` `adamolsson docker`, and
  `server deploy` with no `--root` — which resolves `<identity home>/.local/share/holosmith` — answering
  `2. files: failed: install -d -m 0700 /home/adamolsson/.local/share/holosmith answered 1 (install: cannot change
  permissions of '/home/adamolsson/.local/share/holosmith': Operation not permitted)`. The same run with the image's
  own passwd answers `deploy: into /home/node/.local/share/holosmith` and step 2 `changed: 7 actions`, so the mount
  would turn a working command into a permission refusal while buying the spelling of a line this shape never prints:
  the container's account is in the socket's group by `--group-add` whatever the passwd says, so `may_open` answers
  yes and no `usermod` is printed. That spelling is not the mount's only effect, though: for an invoking uid the
  image's passwd does not name, the account read itself fails — and that is a precondition of this shape rather than
  something a mount fixes, so "What is not measured" names it. The group file is therefore the one identity file
  mounted, and it is the one the failing assertion needed.
- **the `docker` CLI and its CLI-plugin directory, at their own paths.** No image ships either: the CLI is a 47 MB
  binary on this host, and an image that carried one would be a Docker host, which this one is not. `docker build`
  needs the buildx plugin, and `holosmith build`, `case_smoke` and `case_server_images` all run it. Both directories
  are derived from the CLI's own location, because that is where docker searches for its plugins
  (`<exec_root>/lib/docker/cli-plugins`, `<exec_root>/libexec/docker/cli-plugins`).
- **the tree at its own absolute path, the tree's git dirs and the cleanup's roots at theirs.** Measured: a
  container that had the tree at `/repo` asked the daemon to mount `/repo/pins/pi.txt` and got an empty root-owned
  directory, with no error anywhere, because a bind source is resolved in the *host's* namespace (ADR-0035); with the
  same-path mount the file read. Every case that hands the daemon a path — `$repo_root/target/verify-<case>-<pid>`
  scratch trees, `case_upgrade`'s data root, the smoke root — depends on that. The tree is not the only host path the
  daemon resolves, which an earlier version of this paragraph claimed: in a linked worktree `.git` is a pointer file to
  `<common>/worktrees/<name>`, so the git dir and the common dir are two more, and every root the gate's own cleanup
  reads is the rest, because it locks, claims and sweeps under each one. All of them are mounted below and named in D2.
- **the emulator, when the machine has one**, mounted at `/run/verify/<its name>` and forwarded as that in-container
  path. No image ships one, and `scripts/check-static.sh` prints `SKIP aarch64-unknown-linux-musl execution` without
  it — a SKIP the host run does not print. Measured in the probe container: `qemu-aarch64 version 7.2.0` run from the
  mounted path, and this tree's aarch64 artifact answering `version 0.1.0` under both the mounted emulator and a copy
  of it, which is the empty-root case.
- **the invoking uid and gid, never 0.** The artifact refuses a root invocation before it parses argv (ADR-0090), so
  root here would be a gate that fails at its first boot with a refusal that reads like a defect in the tree. The
  session cases already assume the gate's uid is theirs; measured in the probe, `id -u` inside is 1000 and
  `/home/node` is a writable `HOME`.
- **`TMPDIR` on a directory that outlives the container**, which D2 is about.

Measured in one probe container with that flag set: `docker client=29.9.2 server=29.9.2`, `docker buildx version`
answering through the mounted plugin directory, `flock` and `git 2.39.5` present, the tree's `Cargo.toml` readable and
owned by 1000:1000, and a `docker build` plus run of a throwaway `FROM scratch` image succeeding — the daemon does the
building, so a container that cannot reach a registry can still build.

## D2 — `TMPDIR`'s mount point is decided by the socket cap, not by taste

`scripts/verify.sh`'s work dir is `WORK="$(mktemp -d)"` — a directory under `TMPDIR` — and a failing run's evidence is
that directory, which it names in the `work dir kept:` line. So `TMPDIR` has to be a directory on this machine that
the container mounts, or a failed run loses the evidence it just told the operator to read.

It is `<repo_root>/target/tmp-verify-in-container` on this machine: created by the wrapper, mounted, printed in the
wrapper's first line, and **never removed by the wrapper**, so the evidence survives the container the same way it
survives a failed host run (the next run's cleanup is what clears a `target/` scratch tree, ADR-0089).

Its **mount point inside the container is `/verify-work`**, and that is arithmetic, not preference. `unix_socket_fits`
caps an `AF_UNIX` name at 107 bytes and the name it audits is `$WORK`-relative, so `$WORK`'s depth comes out of that
budget:

| path | bytes |
| -- | -- |
| a 74-byte checkout (the worktree this was measured in) | 74 |
| `<repo_root>/target/tmp-verify-in-container` | 105 |
| `mktemp -d` under it → `$WORK` | 120 |
| `$WORK/docker-doctor-quick.sock` (the longest of the fourteen guarded `$WORK` paths, 25 bytes with its slash) | 145 |
| `/verify-work` mounted instead → `$WORK` | 27 |
| …and the same socket name | 52 |

Fourteen cases print `SKIP <case>: the socket path is N bytes and AF_UNIX caps one at 107` when their guard does not
fit, so a same-path `TMPDIR` would make the in-container run print fourteen `SKIP`s the host run (whose `TMPDIR` is
`/tmp`, `$WORK` 19 bytes) does not print — the one difference the ticket forbids. That longest name leaves `$WORK`
82 bytes, i.e. `TMPDIR` ≤ 67, and no path under a 74-byte checkout is. Shortening the container-side path is the whole
fix, and the mapping it costs is one line on screen: the wrapper's first line names both, and a failed run's
`kept work dir: /verify-work/tmp.XXXX` is `<repo_root>/target/tmp-verify-in-container/tmp.XXXX` here.

**This is not an exception to the same-path rule, and the reason is what the rule is about.** The rule exists because
a *mount source* is resolved in the host's namespace: the tree, the `target/` scratch repositories a case makes, a
case's own data root, the smoke and upgrade roots. `$WORK` is never one of them — `scripts/verify.sh` has no
`-v "$WORK…"` anywhere, and the comments at its two `mktemp`-as-mount-source near-misses (`case_upgrade`'s data root,
`case_smoke`'s smoke root) say the case that needs a daemon-visible path builds it under `target/` instead. A
container-side `/verify-work` therefore cannot turn an inner bind into an empty directory: nothing inside ever asks
the daemon to resolve it. The tree keeps its own absolute path, and the wrapper's refusal for a tree the daemon cannot
see is about that path — the git dirs and the cleanup's roots below are the other mounts the daemon resolves.

**The tree's git dirs are that rule's second and third paths, and a linked worktree is where it bites.** A worktree's
`.git` is a 107-byte pointer file, `gitdir: <common>/worktrees/<name>`, so mounting the tree brings the pointer and not
one object, ref or index it names. Measured in a container with the tree mounted and the git dirs not: `git -C
"$repo_root" ls-files -z` answers `fatal: not a git repository: <common>/worktrees/<name>`, and the pipeline two cases
build on it — `git -C "$repo_root" ls-files -z | tar … | tar …` in `case_agent_toolchain` and
`case_agent_offline_build` — fails through `set -o pipefail` with exit 128 rather than SKIPping. With both paths mounted
at their own absolute paths the same pipeline copied 386 tracked files, and `git status --short` exited 0. So the wrapper
asks git for the two paths (`rev-parse --absolute-git-dir` and `rev-parse --path-format=absolute --git-common-dir`)
rather than reading the pointer file, and skips either one that already resolves under the tree — a normal checkout,
where both are inside it, mounts exactly what it did before.

They are mounted **read-write**, and that is measured rather than assumed. Nothing the gate runs on the tree needs to
write there (`ls-files`, `status`, `describe`, `rev-parse` all answered 0 with the git dirs read-only), but git's refresh
of the index does: a stat-dirty file made `git update-index --refresh` rewrite `<common>/index` under a read-write mount,
and answer `fatal: Unable to create …/index.lock: Read-only file system` with exit 128 under a read-only one. A mount that
turns a git write into that message is a difference between the two shapes that no case would name, and the tree beside
it is read-write by necessity anyway, because the gate writes `target/`, `dist/` and `web/node_modules` into it.

**The cleanup's roots are that rule's fourth set of paths, and one of them is what an in-container run found first.**
The gate's cleanup keeps its state per *repository* rather than per tree — `scripts/gate-cleanup.sh` takes a lock at
`<root>/target/gate-cleanup.lock`, because a run sweeps its sibling worktrees' orphans too, and reads its roots from
`git worktree list`, which names the main root and every linked worktree beside this one. Measured in a full in-container
run with the tree and both git dirs mounted and the main root not: the cleanup's first `mkdir -p '<main>/target'`
answered `mkdir: cannot create directory '<main>/target': Permission denied`, `--claim` claimed no record, and the run's
second line was `warn gate-cleanup: no run record was claimed; a killed run of this one will not be cleaned up` — the
marker ADR-0089 finds a killed run's children by, lost on every run of this shape. With the root mounted at its own path,
`mkdir -p <main>/target` exits 0 and `--claim` writes `<repo_root>/target/verify-run-<pid>/owner`.

The mount list is therefore **the cleanup's own source**, not the common dir alone: `git -C "$repo_root" worktree list
--porcelain`, one `worktree ` line at a time, each mounted read-write at its own path, skipped when it already resolves
under the tree (a normal checkout's roots all do, so nothing is added there) and de-duplicated. The common dir's parent
remains as the fallback for a git that cannot list — `<common>/.git` stripped of that suffix, and a common dir of any
other shape is a tree the cleanup gives no root of its own, so nothing is mounted for it. The list is the point rather
than the main root: a second checkout of this repository under `<main>/../worktrees/<other>` exists on the host and not
inside the container, and the cleanup refuses on its first root that is not a directory — `not a directory: <path>`, exit
2 with empty stdout — *before* the lock and before `--claim`; measured in a probe container with this wrapper's argv and
one root's `--volume` removed, `--dry-run --claim` answered exit 2 with empty stdout. That consequence is **red, not
degraded**: `scripts/verify.sh` leaves `HOLOSMITH_GATE_RUN` unexported, and `case_gate_cleanup` asserts that value at
`wiring/path`, so the in-container gate fails on a multi-worktree machine where the host run is green. The mounts are
**read-write**, and here that is not a preference: everything the cleanup does under a root is a write — the lock file,
the claimed record, and the `target/verify-*-<pid>` directories it removes under each root.

## D3 — `--init` is in, `-i` is out, both measured

**`--init`.** The gate is stopped with signals — its own `EXIT` trap, a `kill -TERM` on the client, the gate's
`kill -9` cases — and a container's PID 1 is not an ordinary process. Measured twice, in the same image: without
`--init`, `docker kill --signal TERM` of a container whose PID 1 was bash left it `Up 5 seconds` — bash installs no
`TERM` handler and the kernel does not deliver the default disposition to PID 1 — and with `--init`, tini forwarded
the same signal and the container `Exited (143)`. Without it, the SIGTERM shape the gate is written for does not
happen, and the same PID 1 is what would have to reap the children a killed run leaves.

**`-i` stays out.** It is only worth having if something reads the wrapper's standard input, and nothing does: every
`read` in `scripts/verify.sh` reads a redirection (`<&"$fd"`, `<<<`, `< <(…)`), never file descriptor 0. A closed
stdin is therefore the same gate as an open one, and a flag that suggests otherwise would invite a case to depend on
a terminal that is not there.

## D4 — what it refuses, and what it forwards

A refusal is **one sentence naming the fix, on stderr, exit 2**. Two is a misuse and 1 is a red tree, so an operator
can never read "could not start" as "the tree is broken":

- running as root → run it as the account that owns the tree (ADR-0090);
- no `docker` CLI, or no daemon answering at `DOCKER_HOST` → install or start one, or run `scripts/verify.sh` here;
- a `DOCKER_CONTEXT` naming a daemon while `DOCKER_HOST` names none → unset it and point `DOCKER_HOST` at that daemon's
  endpoint. `DOCKER_HOST` wins over `DOCKER_CONTEXT` when both are set (measured with the two exported), but a context
  alone has no socket to mount, so the checks here would ask the context's daemon and the container the default one;
- the image is not on the daemon → `holosmith server deploy` builds it, or `HOLOSMITH_VERIFY_IMAGE` names another;
- the tree the daemon cannot see → run this on the machine that owns the daemon, from a path its daemon reads;
- `Cargo.toml` names no workspace version → no tag to default to, so name the image.

The tree refusal is the ticket's own failure and it is measured rather than guessed: one container whose only job is to
report whether the file a Holosmith tree cannot exist without is readable, from the same-path mount the gate uses. A
heuristic on the path would be a guess about a namespace only the daemon can answer for, and an image without a shell
reads as this refusal too (it cannot run the gate, and its own first line would say so).

The image defaults to `agent-image:<the version in Cargo.toml>` — the tag `server deploy` builds — read from the file
rather than from the artifact, which need not exist before a gate run. `HOLOSMITH_VERIFY_IMAGE` overrides it.

The environment is not inherited: `docker run` starts from the image's own, so what is forwarded is a short list of
machine-local inputs no case can derive, and each forward is for a fact the container cannot otherwise know:

- `HOLOSMITH_TEST_IMAGE` — which image the container cases measure.
- `HOLOSMITH_QEMU_AARCH64` and `HOLOSMITH_QEMU_X86_64` — the **in-container** path of the emulator mounted at
  `/run/verify/<its name>`, because that is the file the container can execute. `scripts/check-static.sh` reads each as a
  command and guards on `command -v`, so forwarding the host's path would resolve to nothing and print
  `SKIP aarch64-unknown-linux-musl execution` — a SKIP the host run does not print. Forwarding a host path was the bug
  this shape had; the mount is what makes the forward mean something.
- `DOCKER_HOST`, when it is not a unix socket — a daemon on this machine is reached through `--network host` and
  resolves this tree because the filesystem is the same one, and a remote daemon ends at the tree refusal, which is the
  sentence that says so. `DOCKER_CONTEXT` is refused rather than forwarded (the list above).

Everything else (`HOLOSMITH_GATE_RUN`, `HOLOSMITH_REQUIRE_WEB`, `HOLOSMITH_GIT_TAG`, `HOLOSMITH_CRATE_SET`,
`HOLOSMITH_LOG`, …) is a script's own internal, and leaving it out is what keeps the outer shell from changing what the
gate in there decides.

## D5 — the shape measures the image as a machine, so a program the artifact spawns is the recipe's to carry

A gate in this image measures the image the way a host run measures the host: every program the artifact spawns is a
program that has to be there. Measured in the first full in-container run, 145 cases green and then
`FAIL case_smoke/exit — expected '0', got '1'`: `smoke`'s liveness check and its stop are `kill -0` and `kill -TERM`
**children** (`src/smoke.rs`), the image had no `/usr/bin/kill`, and `node:24-bookworm-slim` carries no `procps` — so
`Command::new("kill")` found nothing to exec, `alive()` answered false on its first poll, and a live `serve` was
reported as `the candidate exited before printing its banner (pid N is gone)`. `type -a kill` said `kill is a shell
builtin` all along, which is why a host run never saw it: a builtin is not a program `Command::new` can find, and the
host has `/usr/bin/kill` anyway.

The image gains `procps`; the wrapper does not gain anything for it. The decision is that this set belongs to the
recipe rather than to the shape, so a subject that runs this artifact anywhere in the image is a subject that has it —
`case_smoke` here, `case_agent_offline_build` there, a session's own container. **Rejected: mounting the host's
`/usr/bin/kill` into the gate container.** It would make the shape depend on an ad-hoc host binary, it would fix
nothing for any other subject that runs this artifact, and it would hide the question worth asking — what else does the
binary spawn that this recipe does not install. Audited against every `Command::new` in `src/`, that question has one
answer: `install`, `chmod`, `id`, `df`, `du`, `openssl`, `stty`, `sh`, `git`, `uname`, `sha256sum`, `file`, `readelf` and
`rm` (coreutils, which the gate reaches through `uninstall`) were already in the image, `tailscale`, `systemctl`,
`journalctl` and `loginctl` are stand-ins the gate supplies itself and `usermod` is printed rather than run (ADR-0088),
`ssh` is neither in this image nor reached by it — the one case that runs it stands its own in on `PATH`
(`scripts/verify.sh:10802`) — and `docker` is the one program the wrapper hands over (D1).

## What is not measured

- **A daemon that cannot see the tree.** The refusal's trigger is measured — the same probe command against a working
  directory holding no `Cargo.toml` answers exit 1, and against this tree exit 0 — but this host's daemon shares the
  filesystem, so no path here makes a local daemon blind. The refusal is for the remote-daemon case, and a remote
  daemon is not something this machine can be made into.
- **The image's crate marker, which is a precondition rather than a measurement.** The gate in there runs
  `scripts/gate-quick.sh`, whose `check-crate-set.sh` finds `/opt/cargo-vendor/.holosmith-cargo-lock` (absent on the
  host, where the check passes vacuously) and exits 3 when the image's vendored set came from a different `Cargo.lock`
  — so the image must be the one this tree's lock built, or `case_gate_quick` reports a stale image where the host run is
  green. Nothing here compares the two digests; the image a deploy builds from this tree is the one that matches.
- **The invoking uid has to be one the image's passwd names.** The image's only non-system entry is `node`, uid 1000, and
  `deploy`'s account read is `id -un` through an `ask()` that treats a non-zero status as no answer — so a run as any
  other uid fails step 2 `named no account`, and the partial deploy `case_server_deploy` runs is unshimmed, which makes it
  `FAIL case_server_deploy/partial/step2` rather than a SKIP. Mounting a generated passwd line would fix it and is a
  bigger decision than this shape; `/etc/passwd` from the host is the one measured to cost more than it buys (D1).
- **The run inside the container is the parent's proof, not a probe's**: what a probe container measured is recorded
  in D1 and D2, and the acceptance for this shape is a full run ending `verify: <n> assertions, ok` whose `SKIP` lines
  are a subset of the host run's.
- **The wrapper's own contract has no `case_*` in `scripts/verify.sh`**, and that is a decision rather than an oversight:
  every refusal it can reach costs a container boot inside the very run being measured, and the half a parked case could
  check cheaply — argv forwarding and the exit status — is `exec docker run` with nothing between, which a full run proves
  by being a full run. The gap is named here instead of being tested from the inside, and criterion 3's run is the
  evidence.
- **A killed wrapper's container.** A `SIGKILL` of `docker run` leaves the container running, and it carries no
  `HOLOSMITH_GATE_RUN` marker (D4), so `scripts/gate-cleanup.sh` cannot find it by the rule that finds every other
  child (ADR-0089). The marker the gate in there claims names a pid in the container's own namespace, so a later host
  run reads it as a live run and spares the record directory: a killed run leaves one `target/verify-run-<n>` and one
  container for a human to notice, not a broken next run. `--rm` removes the container as soon as its PID 1 exits.

## Consequences

- `scripts/verify-in-container.sh` exists, is executable, parses nothing and forwards every argument and every exit
  status to `scripts/verify.sh`; `scripts/verify.sh` itself does not change.
- The wrapper adds one `docker run` per invocation before the gate: a `--network none` probe that asks the daemon
  whether it can see this tree.
- It mounts the tree's two git dirs at their own paths, every root `git worktree list` names for the gate's cleanup,
  and — when the machine has them — the aarch64 and x86_64 emulators at `/run/verify`, so a linked worktree's
  `git ls-files`, the cleanup's claim and sweep, and the non-native artifact measure in the container as they do here.
- `AGENTS.md` and `agent.Dockerfile` stop saying full verification is a host's work: the two shapes are named, and
  what stays true is what the image's header now says — a *session's* container carries no daemon, and the gate's
  container gets one handed to it for the length of a run.
- Nothing in the product changes: no Rust, no CLI verb, no config key. The wrapper is an adapter over a script the
  gate already had.
