# The container is the agent: an attached `docker run`, pi's RPC on its stdio, and a fifth mount

HS-031 is the story that turns "a container exists and is hardened" into "a container is an agent": pi runs in
RPC mode inside it, the server detects "ready" from a protocol fact, and the exit code and stderr that HS-030
built a classifier for finally have an observer. Spec item 2 is the launch and the environment, item 6 the run
line and the exit rules, V8 the shapes the mapping layer must survive, and open question 9 the project-trust
decision this story closes.

## The launch is attached, and `--detach` and `sleep infinity` are gone

Spec item 6 writes `docker run -i --rm --init --name s_<id> --network agents_<id> …`, and item 2 launches
`pi --mode rpc`; together they say the container is **not** detached. `container::SessionContainer::run_args`
therefore drops `--detach`, gains `-i`, and ends with `pi --mode rpc --no-session --approve --model <row>
--thinking <row>`. `IDLE_COMMAND` (`sleep infinity`) is deleted: it existed so that "the flags are right" was
inspectable before there was an agent to run, and the story it named is this one.

The command is spawned as a `tokio::process::Child` with piped stdin/stdout/stderr (`src/agent.rs`), which is
what gives the server three things at once:

- **pi's RPC channel**: one JSON object per line, `\n` as the only separator (`docs/rpc.md`). The reader is
  `AsyncBufReadExt::read_until(b'\n')` — a byte split, never a line reader that also splits on `U+2028`, which
  is legal inside a JSON string and which Node's `readline` gets wrong.
- **a "ready" signal that is a fact, not a sleep**: the handshake sends `{"id":"ready-1","type":"get_state"}`
  and accepts only the `response` record that repeats that `id`
  (`docs/rpc.md`, "Correlate commands and responses"). `success:false` is the agent's own words; silence is a
  failure after `READY_TIMEOUT` (30 s). It is the pi step, so the session's `starting → idle` happens after
  it, through the one state function as before.
- **the exit code and stderr**: an attached `docker run -i` exits with the container's code (measured: `exit
  3` in the container answers `3`; a `docker rm -f` answers `137`; a `docker stop` answers `143`).

## Two readers, because both pipes block the process

`docs/rpc.md` says to read stdout continuously, and the same is true of stderr: a pipe nobody drains stalls the
writer. So both readers start at launch. stdout's records that this build does not map — session events,
extension UI requests, pi's own bookkeeping — are counted and dropped rather than treated as errors, which is
the same rule HS-032 and HS-035 will need. stderr is kept as a **bounded tail** (`STDERR_TAIL`, 64 KiB): the
whole of it can be a build log, and both of its consumers (the row's reason and `session.log`) are about the
end.

The exit is reported **after** the stdout reader has finished, so a caller cannot be told the process ended
before it read the answer it was waiting for.

## The agent directory is a fifth mount, at its own host path

Spec item 2 puts pi's configuration in `<data>/sessions/<id>/pi-home`, and D1's "spec edits that follow from
D1" spell the mount `-v <data>/sessions/<id>/pi-home:/home/agent/.pi/agent`. **This build mounts it at its own
host path** and points `PI_CODING_AGENT_DIR` there, for two reasons that are already this repository's rules:

- **Host path and container path are the same string is item 1's rule**, and ADR-0035's argument about
  `--user` applies to a mount destination as well: `/home/agent` presumes an image user this build does not
  choose. The image in this machine's gate runs as `node` (uid 1000). Docker creates a **missing destination**
  as a root-owned directory, so the spec's path would hand pi a directory it cannot write, and the failure
  would arrive as a mysterious handshake timeout.
- the mount is **writable** because pi writes into it: the first launch this was measured with created
  `auth.json` and `models-store.json` beside the seed.

That makes `<data>` a path the **daemon must be able to see**, exactly as `<projects>` already is (ADR-0035):
a data root on a tmpfs the daemon does not share gives the agent an empty directory and no error at all. The
gate's Docker cases and `tests/agent.rs` therefore build their data roots under `target/`, and this is the one
new environment constraint the story adds.

**The seed is written once.** `settings.json` comes from an embedded template
(`crates/holosmith/templates/pi-home/settings.json`, `include_str!`) with the row's `model` and `thinking`
patched in, and only when the file is absent — a resume must not overwrite what pi itself wrote. The template
carries **no secret**, which is a unit test's assertion rather than a promise; the key proxy's `models.json`
arrives with HS-039.

**The row's model and thinking are also command-line flags**, and both are load-bearing: measured in the
gate's image with `PI_OFFLINE=1`, `get_state` answers `model.id: "unknown"` and `thinkingLevel: "off"` when
neither is on the command line, the real values when they are, and the seeded settings file alone does not
resolve them. So the file is the session's durable configuration and the flags are how this build makes pi
honour it offline; neither is a duplicate of the other, and HS-039's `models.json` will sit beside the file.

## Project trust is `--approve`, and the risk table's row is now a decision

Print, JSON and RPC modes cannot show pi's trust prompt (`docs/security.md`, "Project trust without an
interactive prompt"), so the story's open question 9 has exactly two answers: grant it, or configure
`defaultProjectTrust: "always"` in the seed. **`--approve`** is the choice:

- it is per process, so nothing about trust is written into a directory that could be reused by a different
  project;
- it is visible in the launch line, so "which decision did this session run with" is answerable from
  `session.log`;
- and the alternative is a setting in the seed, i.e. a second place a launch's behaviour is decided.

What it means is stated in the risk table and unchanged: a project's own `.pi` resources execute **inside the
container**, with the agent's own (dropped, no-new-privileges, non-root) capabilities, and V8/HS-035 exists
because a project-local extension can then ask for UI input that RPC mode cannot show. The test proves the
mechanism rather than the intention: `tests/agent.rs` writes a project-local prompt into the worktree, asks
the live agent for `get_commands`, and finds it — pi loads project resources only for a trusted project.

## The exit is the server's to record, and a stale one is not news

A backend may emit frames and may not write a row (ADR-0019), so the exit goes to server code through one new
trait method:

```text
SessionBackend::supervise(&self, session) -> Option<ExitWatcher>   // Some once, after a launch
ExitWatcher::wait() -> ProcessEnd::{Exited { code, stderr }, NotCurrent}
```

The provisioner spawns the task that awaits it and calls `exit::Exits::record` — ADR-0038 named HS-031 as that
caller — and `None` means "this backend has no process to supervise" (the stub), which is not a failure.

**ADR-0038's inherited ordering constraint is closed by keying the flag to the container's incarnation**, its
second option. Every launch of a session bumps a per-session **generation** in the backend's registry, the
registry entry is replaced and never removed (a forgotten entry would let a re-created id restart the count
and call an old exit current), and the waiter compares the generation it was launched with against the
registry *at the moment the exit arrives*. So every interleaving has one answer:

| The old container's exit arrives… | The flag | The answer |
| --- | --- | --- |
| before the resume's container step | set (the rung's `Stopper` wrote it) | `Exit::Expected` — no row change |
| after it (the flag is already cleared) | clear | `NotCurrent` — dropped, because a newer container is current |

The alternative ADR-0038 offered — making the ladder await the report between its rungs — would have put a
wait into the one path that must not block, and would have needed the watcher in two owners. The generation is
one number and one comparison.

**A delete forgets the launch before it removes the container**, and that is what makes its own `docker rm -f`
quiet. Not the expected-stop flag: the state table has no `unexpected_exit` row out of `deleting`, so a
recorded exit could at worst log a refused transition — and `Exits::record` appends to `session.log` *before*
it classifies, which would re-create the session directory the delete removes a line later. Dropping the
registry entry answers [`ProcessEnd::NotCurrent`] instead, so nothing is appended and nothing is classified.
`Stopper` keeps owning the flag where it means something: a stop of a session that will still exist.

## The launch line, and why nothing is redacted

`session.log` (item 15, HS-030) gets one line before the process starts:

```text
launched <image> (pi <version>) for session <id>: pi --mode rpc --no-session --approve in <agent dir> — model
<model>, thinking <thinking>, project trust approved
```

It is **built from an allow-list of fields**, so there is nothing to scrub: no environment value, no key and
no operator-supplied command line can reach it. A filter over arbitrary text is a filter that misses the next
secret somebody adds; a line assembled from seven named facts cannot leak an eighth. A failure to write is not
a failure to launch, and the log writer is `exit::SessionLog` — lifted out of `Exits` for this, the same
function it always was.

**stderr reaches `session.log` once**, from `Exits::record` at exit. Streaming it live would append every line
twice (ADR-0038's "what it must not do is append the same lines twice") and is a different contract; the row's
reason and the complete log are what a failed session needs.

## What the version assertion does, and what it cannot

`docker run --rm --network none --entrypoint pi <image> --version` is compared with `pins/pi_version()`
(`pins/pi.txt`, the only copy of that literal). It is asked **once per process** and cached, including a
failure: a machine whose image is wrong is wrong for every session, and the operator needs the sentence once
per row rather than one container start per session. It runs at the start of the container step — the step that
launches pi — so a mismatch costs no container, and its sentence names the image, both versions and the fix.

A **mismatch** is proved by `agent::version_sentence`'s unit test and by `tests/agent.rs`'s failure path (an
image that cannot answer at all), not by a second image with a different pi, which this machine cannot build.
Recorded here rather than papered over, the same way ADR-0029 recorded HS-025's `ahead` half.

## What this story does not decide

- **A resume is a new agent process.** `--no-session` means pi keeps no session file, so item 16's "the same
  session id reopens the pi session" is not what this build does; Holosmith's event log is the transcript, and
  HS-032 owns the revisit when a turn exists. **ADR-0040 records the answer HS-032 gave**: the transcript is
  Holosmith's, pi is not told any of it, so a resumed agent starts with no memory of the earlier turns and
  replaying the log into a new pi is a story of its own — not something a mapping can invent.
- **Nothing maps a turn yet.** stdout's non-`response` records are counted and dropped (HS-032/033/035).
  **Superseded by ADR-0040:** they are published to the session's turn on a bounded channel (backpressure, not
  loss) and `crate::rpc` maps them; HS-033 and HS-035 extend the mapping rather than the reader.
- **A crashed server leaves its container** (the CLI's death does not stop it), which is HS-044's reconcile;
  the next start's `rm -f` is what makes it boring in the meantime.
- **The container appears seconds after the step returns** — measured at several seconds on this machine. So
  the container step's success means "the process was spawned", the pi step's means "pi answered", and
  anything that inspects a container must wait for it (`tests/*`'s `wait_running`, the gate's `idle`).

## What a reviewer should be able to check

- `tests/agent.rs` launches a real container: the vector's command and environment are in `docker inspect`,
  the seed is on disk, the launch line is in `session.log`, the project's own prompt is in `get_commands`
  (trust), and the exit arrives as a code. Both tests skip without a daemon and an image.
- `tests/container.rs` still proves V1, the limits and the start lock, over an **attached** container: it now
  waits for the container to come up, because the step returns before the daemon has created it.
- `tests/provisioning.rs`'s `a_scripted_agent_exit_reaches_the_row_through_the_server` proves the wiring —
  provisioner → watcher → `Exits::record` → the row — with the stub's scripted exit, no Docker.
- `an_exit_is_news_only_for_the_launch_that_is_current` proves the generation rule three ways without Docker:
  the current launch's exit is news, a replaced launch's is not, and a forgotten one's (a delete's) is not. The
  fake agent ends when a **file** appears, so the test decides when the exit happens and the comparison is not
  raced.
- Mutations worth running: deleting the generation comparison makes that test report `Exited` for a replaced
  launch; deleting `forget_agent` from `delete` makes the third case report `Exited` over a delete's own
  removal; deleting `--approve` makes `tests/agent.rs`'s `get_commands` assertion fail; deleting
  `--model`/`--thinking` makes the ready handshake's log line report `unknown`; making `version_check` ignore
  its argument makes its unit test pass both arms with one answer.
