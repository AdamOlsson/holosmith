# The two measurements that needed bubblewrap are containers

Two gate measurements used `bwrap` to hand a process a filesystem it did not have on this machine, and both are
containers now.

- **The empty root** in `scripts/check-static.sh`: the artifact (and the emulator, when one is in play) run from a
  filesystem holding nothing else, which is what proves it has no dynamic loader and no userspace dependency. Built as
  a `FROM scratch` image per target and run with `--user $(id -u):$(id -g)`.
- **The account fixture** in `case_install` and `case_install_terse`: `holosmith install` needs a `/etc/passwd` whose
  entry for this uid names a home the case owns, so that the unit `--config` composes is the one the account's own
  manager would resolve (ADR-0091 D4). That file is now the container's `/etc/passwd`, and the container runs as this
  gate's uid and gid.

Nothing else about either measurement moves: the empty root still reads the banner, the `database /data/app.db schema
<n>` line, the listing under the data root before and after the stop, and `cmp`s the served page against
`web/dist/index.html`; the two install cases still assert the six steps, the two manager commands' argv, the unit's
bytes and mode, the second run's silence, and the terse report byte for byte.

## Why bubblewrap could not be kept

Because the gate is moving **inside** a container — `case_agent_offline_build` already runs `scripts/build.sh` in
`agent-image` under `--network none`, and a session does its own work the same way (ADR-0105, ADR-0106) — and `bwrap`
cannot make its namespace there at all. Measured on this host, the three layers that stop it:

- the kernel restricts unprivileged user namespaces (`kernel.apparmor_restrict_unprivileged_userns=1`), and Ubuntu's
  AppArmor profile for `bwrap` is what grants the host's own binary its uid mapping;
- Docker's default seccomp profile blocks the namespace clone outright, and its AppArmor profile is `docker-default`
  rather than one that names `bwrap`;
- the combination that did work needed a named AppArmor profile, `CAP_SYS_ADMIN` and an unconfined seccomp profile.

That is three privileges to keep a tool a container makes unnecessary, on a machine that has a Docker daemon anyway
for every other fact in this gate. So the sandbox became an image, and the image became the thing the daemon already
knows how to make.

## D1 — the empty root is a `FROM scratch` image, per target

The build context is a temporary directory under `target/` holding `holosmith` (and `qemu` when an emulator is in
play) and a two-line Dockerfile:

```dockerfile
FROM scratch
COPY holosmith /holosmith
```

`target/` because a bind source has to be a path the daemon can see: a `mktemp -d` under a tmpfs `/tmp` becomes an
empty root-owned directory with no error anywhere (ADR-0035). The image is tagged for this run and the target, and it
is removed once its check has passed.

Three things the run needs, each stated where it is made:

- **`--user $(id -u):$(id -g)`**, because a scratch image defaults to uid 0 and `main` refuses root before `Cli::parse`
  (ADR-0090). Measured: a scratch image holding a program that prints `getuid`/`getgid` answers `1000 1000` under
  that flag on this host.
- **`--network host`**, so the `/dev/tcp` fetch of `127.0.0.1:$port` is the same fetch it always was: the container
  binds in the host's network namespace, and the data dir is still bound in so the listing before and after the stop is
  read from the host's side.
- **`--backend stub`**, unchanged: this run is about the artifact's own linking, not about a machine's choice, and the
  image has no daemon, no `git` and no `df` — which is the point of the empty root.

The empty-root claim is now **asserted** rather than implied. `FROM scratch` plus one `COPY` leaves the image no shell
to run and no loader to find, so what ran there is the artifact alone; the check says so by asking for the shell a
userspace dependency would need and requiring the failure (`exec: "/bin/sh": stat /bin/sh: no such file or directory`,
measured), and `file` and `readelf` on the artifact keep proving the loader half.

The stop is still a `SIGTERM` whose only assertion is that the process ended. Measured: `kill -TERM` on the client of
that `docker run` forwards the signal to the container's own process, its handler runs, and the client exits with the
container's status — 0 for a handled `TERM`. So the check keeps discarding the status, and ADR-0007's sentence
crediting `bwrap`'s 143 is wrong now and amended there.

## D2 — the account fixture is a container with the case's own `/etc/passwd`

```text
docker run --rm --name "hs_install_$$_<case>" --network host --user "$run_uid:$run_gid" \
  --entrypoint "$artifact" \
  --volume "$root:$root" --volume "$passwd:/etc/passwd:ro" --volume "$artifact:$artifact:ro" \
  --env "PATH=$shims:$image_path" --env "HS_…=$logs/…" "$image" "$@"
```

- **The passwd file is the case's**, and it holds `root` and one `operator` entry for the measured uid. The `nobody`
  entry that used to sit beside it is gone: it existed for a user namespace's overflow uid, and a container's `--user`
  maps the uid it names.
- **`--user "$run_uid:$run_gid"`** from `id -u`/`id -g`, with the same non-zero assertion `fake_uid` carried: a `0`
  here would mean the case had become the refusal rather than the install.
- **`--network host`**, because the responder answering `/api/health` is the case's own python process on the
  loopback, and step 6 plus the wait after `enable --now` are facts about reaching it.
- **`--entrypoint "$artifact"`**, because the image's entrypoint is `pi`. The artifact is also bound read-only at its
  own absolute path, so a deploy that plants the release it is running (ADR-0094) has the bytes to plant.
- **Every path is bound at the same absolute path on both sides**, so the home the deploy writes, the unit it composes
  and the shim logs it appends are the ones the case reads with `stat`, `cmp` and `grep` on the host. Each source is
  under `target/` or `dist/`, which the daemon can see.
- **The shims are ahead of the image's own `PATH`**, read from the image's config rather than assumed, so the deploy's
  `install`, `chmod`, `df` and `openssl` are the image's and a `HOLOSMITH_TEST_IMAGE` naming another image does not
  silently drop one of them.

The image is `${HOLOSMITH_TEST_IMAGE:-pi-agent:local}`, which the fixture's config already names as `agent_image`. The
guards are the daemon and the image, each printing one `SKIP` line naming what is missing; `tests/install.rs` and
`tests/serve_cli.rs` stay the arms that need neither.

## What this costs

- **Both arms want a Docker daemon now**, so a machine without one prints a `SKIP` where it used to measure. That is
  the trade this buys and it is not free: `scripts/build.sh` is what runs the empty-root check, so a machine that
  builds a release artifact now needs a daemon to prove it serves — and a session's container, which deliberately has
  no daemon, keeps the empty-root `SKIP` it already printed (`case_agent_offline_build`'s comment says why).
- **One scratch image per target**, created and removed by `scripts/build.sh`'s run of `check-static.sh` on the
  operator's daemon. The script grows an `EXIT` trap so a `fail` between the build and the check's own removal does not
  leave the tag or the half-started container behind.
- **The install cases sweep their containers in the gate's `EXIT` trap**, by a name carrying this run's pid, so a
  failed run does not leave one for the next run of the case to trip over by name.
- **A run killed with `SIGKILL` runs no `EXIT` trap**, so the trap is not the whole answer for the container that
  serves: `--rm` removes it when the serve process ends, and a killed run ends nothing. The run therefore carries the
  gate's own marker into that container — `--env HOLOSMITH_GATE_RUN=<record>` when the gate is what ran the script, and
  no flag at all when `scripts/build.sh` runs it outside one. A container's process is a host process, and `--user`
  makes it this account's own, so `scripts/gate-cleanup.sh` can read its environ and match a whole
  `HOLOSMITH_GATE_RUN=<record>` element against a dead run's record: it signals that process, the serve ends, and
  `--rm` removes the container. That is the rule every other child of the gate is found by (ADR-0089), applied to the
  one child that used to carry `bwrap`'s `--die-with-parent` instead. Measured: `tr '\0' '\n' < /proc/<pid>/environ` of
  a container started with that flag prints the record as one whole element, and the sweep run against a dead record
  kills that pid and leaves no container behind.
- **The aarch64 empty-root arm is measured here through the emulator**, not on an aarch64 kernel:
  `qemu-aarch64-static` is on `PATH` where this gate is run from, so the image is `COPY qemu /qemu` beside
  `COPY holosmith /holosmith`, the run is `/qemu /holosmith`, and the `serve` assertions passed there — the gate's own
  line is `ok aarch64-unknown-linux-musl via qemu-aarch64-static: serves the app from an empty root`. A machine without
  the emulator prints the `SKIP` naming it, which is why the emulator stays a machine-local prerequisite rather than a
  declared dependency (ADR-0005).

## Bubblewrap is gone as a dependency, not as a string

The decision is zero **dependencies**: no procedure in this tree invokes `bwrap`, no script asks whether it exists, and
no prerequisite names it. Residual textual mentions are historical measurements, not dependencies, and they stay:

- ADR-0054 and ADR-0056 record `/proc/1/comm` as `bwrap` on the machine those measurements were taken on, which is a
  statement about that machine rather than an invocation of the tool;
- `crates/holosmith/src/doctor.rs`'s "`/proc/1/comm` can be `bwrap` or `tini`" is an illustration of why the check
  cannot read that file as "there is no systemd".

ADR-0090's measurement recipe is the one that still told a reader to invoke `bwrap`, so it is aligned with the
container `case_root_refusal` actually runs; ADR-0005, 0007, 0080, 0091, 0092 and 0105 stopped claiming the sandbox,
and `AGENTS.md`, `tests/install.rs` and `images/builder-entry.sh` follow.

## What is not measured

- **An aarch64 host's empty root.** This host runs that arm under the emulator, so what served there is the artifact
  on this host's kernel and filesystem, not on an aarch64 host's.
- **A daemon-less host's empty root**, which is a `SKIP` and not a measurement at all.
- **A real user manager and a real daemon socket inside the install fixture.** The container is a machine of the
  case's own, and what it still cannot show is systemd loading the unit — the same limit ADR-0091 recorded.

## Consequences

- `scripts/check-static.sh` builds and removes one image per target, runs `version` and `serve` from it, and asserts the
  no-shell half of the empty-root claim; a machine with no daemon or no CLI prints one `SKIP` naming which.
- `case_install` and `case_install_terse` guard on the daemon and the image, run the artifact in a container with the
  case's `/etc/passwd` and this gate's uid and gid, and their containers are swept by the gate's `EXIT` trap.
- A machine with a Docker daemon measures more than it did (a real runtime starting the artifact from a filesystem
  holding nothing but itself) and a machine without one measures less (the empty root and the two install cases are
  `SKIP`s). The daemon is the gate's line between those two machines, not a convenience.
