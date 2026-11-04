# The container story fills the network step, runs detached, and bounds the container with three keys

HS-028 is spec item 6's run line — `docker run -i --rm --init --name s_<id> --network agents_<id>` — plus
"CPU, memory and PID limits come from config with documented defaults" and the negative list: no Docker
socket, no host network, no `--privileged`, no added capabilities. Four things had to be decided for it
that the spec does not decide.

## The network step is this story's, and HS-036 owns the rest of the network

Spec item 6's run line names `--network agents_<id>`, so the container cannot be started without its
network. The step order agrees: the provisioner runs `worktree → network → container → pi`, so a container
step that came first would have nothing to join. HS-028 therefore creates the network with
`docker network create --internal agents_<id>` and removes it on teardown.

`--internal` is the whole isolation: a network without it has a gateway to the host and a route out, so it
is not a flag a caller can pass. What HS-028 does **not** do is the rest of item 2: the `egress` network, the
two proxy containers, the startup self-test that refuses to start sessions when a probe can reach the
internet or the host, the network-off probe, and the startup removal of orphaned `agents_*` networks. All of
that is HS-036's, which extends this step rather than replacing it.

## The container runs detached until HS-031

Spec item 6 spells `docker run -i`: pi speaks RPC over the container's stdin and writes its frames to
stdout, so the shipped launch is an attached process whose stdio the server holds. That is HS-031's
change, and it is a large one — a `tokio` child process, two pumps, a reaping loop and a classification of
how it ended (HS-030). Pretending to do it here would mean an attached process nobody reads from, which is
a container that fills its stdout pipe and blocks.

So this story runs `--detach` and the container's command is [`container::IDLE_COMMAND`], `sleep infinity`.
It is a placeholder and the module says so: the container exists, is mounted, is hardened and is limited,
and nothing inside it is working yet — which is exactly what this build can truthfully claim. The demo is
therefore half artifact and half test (the precedent is HS-025/ADR-0029): the artifact shows a create
reaching the *agent* step, and `tests/container.rs` inspects the real container's flags.

Detached plus `--rm` means a container whose command exits is removed immediately, which is why the
placeholder is a process that does not exit; a container that vanished between `docker run` and
`docker inspect` would make the flags unobservable and the step pointless to assert.

## The limits are three integer keys

`container_cpus` (default 2), `container_memory_mb` (default 4096) and `container_pids` (default 512).
Integers because the config's `Kind` has no decimal and `--cpus 2` is a spelling Docker accepts; a string
key would have needed a parser and its own refusals for `"2.0"`, `"two"` and `"-1"`. Each refuses `0`,
because Docker reads `--cpus 0` and `--memory 0m` as *unlimited* — the opposite of what the key is for — and
a fork bomb is a fork bomb even with `--cap-drop ALL`, which is why `--pids-limit` is one of the three.

## The docker CLI is one module, and it passes `-H` rather than `DOCKER_HOST`

`src/docker.rs` holds a `Docker` value (the daemon's address), a synchronous `run` (called from
`spawn_blocking`, like `worktree.rs`) and the handful of operations the backend needs. Two consequences
worth naming:

- **`-H` rather than an inherited `DOCKER_HOST`.** The config key is where a rootless socket or a TCP
  endpoint is written and the preflight reads the same value; an inherited variable would be a second
  opinion about the same machine. A `docker` that answers is also a fact an operator can reproduce by hand,
  which is what makes a failure diagnosable from the row's `status_reason`.
- **`Mounts::check` runs before `docker run`**, because HS-027 measured that `-v` creates a missing source as
  an empty root-owned directory instead of failing (ADR-0035). The container step is the first caller that
  could be bitten by it, so it is the first caller that asks.

## What is asserted, and by whom

`docker inspect` is the only place a running container's real configuration is visible, so
`tests/container.rs` reads it field by field: `NanoCpus`, `Memory`, `PidsLimit`, `NetworkMode`, `Init`,
`AutoRemove`, `Config.WorkingDir`, `Config.Env` (`HS_SESSION_ID`), `CapDrop` containing `ALL`, `CapAdd`
empty, `SecurityOpt` containing `no-new-privileges`, `Privileged` false, `PidMode` empty, and four `Binds`
with no `docker.sock` among them. The test sets the limits **away from their defaults** so "from config" is
what the numbers prove, then deletes the session and asserts the container and the network are gone.

The gate's case (`case_docker_container`) cannot see any of that — the provisioner takes the container back
the moment the agent step fails, which is ADR-0015 working — so it asserts what the artifact can show: the
row ends `error` at the **agent** step (two real daemon steps succeeded), and nothing is left behind.
