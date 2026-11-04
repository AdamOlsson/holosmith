# RESUME — orchestrator handoff (cold start, after thirty-three stories, with a real Docker daemon)

You are the **orchestrator for implementing Holosmith**. `PROMPT.md` is the standing operating
procedure and still governs; this file is the state of the world — what exists, what the environment
already has, which decisions are settled, and the landmines that cost real time. Read all of it before
touching anything.

The run is **fully automatic**. The operator is not present. Never ask them a question: decide
canonically, record the decision in the Linear issue and the commit message, and continue.

---

## 0. What this project is

`HolosmithSpec.md` is the authoritative specification. A single-operator system that runs coding agents
in containers on machines you own, driven from a phone. The work is sliced into **80 stories**
(`HS-001` … `HS-080`) in `docs/stories/`, one Linear issue each, in five phases:

| Phase | Stories | File |
| --- | --- | --- |
| 0 — walking skeleton | HS-001 … HS-006 | `docs/stories/phase-0-walking-skeleton.md` |
| 1 — session lifecycle (stub backend) | HS-007 … HS-023 | `docs/stories/phase-1-session-lifecycle-stub.md` |
| 1b — Docker backend | HS-024 … HS-046 | `docs/stories/phase-1b-docker-backend.md` |
| 2 — deployable MVP | HS-047 … HS-067 | `docs/stories/phase-2-deployable-mvp.md` |
| 3 — multiple machines | HS-068 … HS-074 | `docs/stories/phase-3-multiple-machines.md` |
| 4 — self-development | HS-075 … HS-080 | `docs/stories/phase-4-self-development.md` |

`AGENTS.md` carries the repository rules, including that **Linear scope is exactly one project**:
`Holosmith` (`P-PER-2`, team `Personal`/`PER`). `VulkanRenderAPI` belongs to another repository — never
read, create, comment on, or close anything there.

**The MVP line** is phase 2's first stories plus, before them, the rest of phase 1: a session can be
created, prompted, aborted, steered, stopped, resumed and deleted, and it is driven from the phone.

---

## 1. Where the work stands

Branch **`master`** (this repo has **no `main`**; do not rename it). The tree is clean, and every
commit is **pushed** (see the note on pushing below). `HEAD` is `0851325`; any commit after it is
bookkeeping — this file's own refresh. Confirmed on `origin/master`: `0402d8f..0851325`.

**Linear: 33 Done, 47 Backlog, all in project `Holosmith`.** Done is `HS-001` … `HS-029`
(`PER-33` … `PER-61`), `HS-059` (`PER-91`), `HS-060` (`PER-92`), `HS-061` (`PER-93`) and `HS-065`
(`PER-97`).

**The Docker block is gone.** A real daemon answers on this machine (§2), so `HS-027` … `HS-058` are
unblocked and the queue is contiguous again from `HS-030` on. Three stories have now been built against a
real `docker run` — the mounts and V1 (`HS-027`), the container with its limits (`HS-028`) and the start
lock with `stop`/`resume` (`HS-029`) — and each is proven by a test that runs a container, not by a
document.

| Story | Linear | Commits | ADR |
| --- | --- | --- | --- |
| HS-001 static binary, `version` | PER-33 | `e481ea7`, `d302635` | 0004, 0005 |
| HS-002 one typed config | PER-34 | `bbb1f0f`, `a45edac` | 0006 |
| HS-003 `serve`, embedded app, `/api/health` | PER-35 | `50c20ee`, `e71da58`, `cc9aadb` | 0007, 0008 |
| HS-004 SQLite + forward-only migrations | PER-36 | `a4774b9`, `bd3b73e`, `ae92a7d` | 0009 |
| HS-005 browser-facing guards | PER-37 | `6c4f02c`, `6994c0f` | 0010 |
| HS-006 `scripts/verify.sh` | PER-38 | `2af15d4`, `108a6ee` | — (0003 governs) |
| HS-007 session row + repository, schema 2 | PER-39 | `ad3ffc0`, `fad974d` | 0011 |
| HS-008 `POST /api/sessions` | PER-40 | `c5891e4`, `858d295` | 0012 |
| HS-009 event log + single writer, schema 3 | PER-41 | `5cae107`, `dae1fd9` | 0013 |
| HS-010 SSE stream, `Last-Event-ID` | PER-42 | `3b03c5c`, `07f318d` | 0014 |
| HS-011 stub backend + provisioner steps | PER-43 | `9071390`, `b5022fc` | 0015 |
| HS-012 one enforced state machine | PER-44 | `9424677` | 0016 |
| HS-013 sessions list + new-session form | PER-45 | `47471c4` | 0017 |
| HS-014 session screen, live transcript | PER-46 | `82c8c35` | 0018 |
| HS-015 prompt and the turn lifecycle | PER-47 | `d4077df` | 0019 |
| HS-016 abort with the full ladder | PER-48 | `d034237`, `3d2ecba` | 0020 |
| HS-017 steer a running turn | PER-49 | `0099ca3`, `8ff1d77` | 0021 |
| HS-018 stopped state and resume | PER-50 | `dce53c4`, `ff9d907` | 0022 |
| HS-019 delete flow | PER-51 | `3d5c415`, `7c4ea02`, `cd85572`, `db95a3f` | 0023 |
| HS-020 cap and eviction | PER-52 | `a3c6825`, `4f37211`, `f67eb6f` | 0024 |
| HS-021 idle stop | PER-53 | `5da2ae2` | 0025 |
| HS-022 turn timeout | PER-54 | `e767757` | 0026 (amends 0020) |
| HS-023 token budget | PER-55 | `2e902c2`, `fbc0659` | 0027 |
| HS-024 Docker preflight | PER-56 | `f241ced`, `0b7d50e` | 0028 |
| HS-025 worktree provisioning | PER-57 | `7e36bde`, `1fe7f6e`, `ce4dc82` | 0029 |
| HS-026 worktree refusal rules | PER-58 | `18e4695`, `c233ab7` | 0030 |
| HS-059 event coalescing | PER-91 | `06f506b`, `65f7107`, `5207a97` | 0031 |
| HS-060 message snapshots and compaction | PER-92 | `5c0f960`, `e2a24b6`, `f0ecf8c`, `901b4ef`, `d1cb620` | 0032 |
| HS-061 history truncation and pagination | PER-93 | `f8c3126`, `315b963`, `0a15876` | 0033 |
| HS-065 daily project backup | PER-97 | `ea313e4`, `b4d7338` | 0034 |
| HS-027 hardened `.git` mounts and V1 | PER-59 | `3025360`, `b009320` | 0035 |
| HS-028 container run with limits | PER-60 | `5aae311`, `b009320` | 0036 |
| HS-029 name conflicts, `stop`, `resume`, start lock | PER-61 | `e826539` | 0037 |

**Next issue: `PER-62` (HS-030 — exit classification)**, then `HS-031` (real pi, `PER-63`) and so on in
`HS-###` order. The whole Docker chain `HS-030` … `HS-058` is unblocked now. What is still behind something
is phase 2 (`HS-047` … `HS-058` are `HS-046`'s, the phase-1 verification run, which is the story that gates
the phase; `HS-066` … `HS-074` sit behind those and behind `HS-049`/`HS-050`), and `HS-075` (`PER-107`),
which is independent of all of it and **still unstarted** — §8's own note carries its four decisions and
three landmines.

Ranking, per `PROMPT.md`: Linear priority (all are `No priority`) → `HS-###` order in `docs/stories/` →
anything already `In Progress` first → skip an issue whose `Blocked by` stories are not `Done`.

### Sixteen facts a cold start must know

1. **A running turn can now be stopped, in two rungs, with or without the way back.** `src/ladder.rs`
   is the shared routine: `clear_queue` then `abort`, wait up to `abort_grace` (the grace *bounds rung
   1 too*), then `running → stopped` with the caller's reason and `stop`. `Ladder::climb` is that plus
   a resume through the provisioner (the abort's path, unchanged); `Ladder::end_turn` is that alone and
   returns `Ending::{Asked, Forced}` (HS-019's delete, which then removes the session instead of
   resuming it). A turn's end is typed (`TurnEnd::{Settled, Aborted}`) so the runner picks
   `agent_settled` or `abort_completed`; a forced stop has **two owners** and the table decides, which
   is why exactly one `StateError::Illegal` in `turn.rs` is a `debug` line. **Two policies now climb it**
   — HS-022's deadline and HS-023's ceiling — and both sit in the runner's own `select!`, each guarded on
   one `fired` so the first to fire owns the reason and no second climb is spawned. The six reason words
   are `forced_abort`, `deleted`, `evicted`, `idle_stop`, `turn_timeout` and `budget_exceeded`.
2. **A turn that will not end is ended for it, and the reason says which policy asked.** The deadline is
   the turn runner's own (`Turn::run`), not a sweeper's, because the row keeps no per-turn timestamp —
   `last_active_at` is activity, so a steer moves it. It races `start_turn` against a sleep and, when the
   sleep wins, hands the ladder a **task of its own** (`Ladder::spawn`) and then keeps awaiting the turn,
   because rung 1 working *is* the agent's `TurnEnd::Aborted`. **A `select!` branch instead of a spawned
   task is a bug**: it drops the climb, and a climb dropped between rung 2's `stopped` and its resume
   leaves a session nobody can bring back. Both endings record `turn_timeout` — the runner's
   `running → idle` and the ladder's `running → stopped`. `turn_timeout_min = 0` builds no timer at all.
   It is the shape HS-023's budget copies: one `fired`, a spawned climb, and a settle that records the
   reason too when it crossed the line between two polls.
3. **The web page has now been rendered, including the paths a frame changes.** Playwright's Chromium
   is inside the sandbox (see §2), and HS-016 … HS-018 drove the sessions list, the create form, the
   session screen, the composer's four actions, the transcript's blocks and boundaries, and the abort /
   steer / resume round trips; HS-019 drove the delete's 202 → 404 and the closed stream; HS-020 drove
   the cap's eviction **both ways** — the row as drawn from the list, and a row turning `stopped` under
   the operator from a `session.status` frame with no reload (which is where that story's one blocker
   lived); HS-021 drove the sweeper's frame path the same way (a row drawn `idle`, then turned
   `stopped · idle_stop` under the operator with no reload). This is still **not** a substitute for a
   gate — nothing in `scripts/` may require a browser —
   but "unverified from source" is no longer the honest answer for a screen that has been looked at.
   HS-060 drove the session screen again after compaction (one block, the whole text, the whole thinking,
   no duplication — the snapshot's `replaceChildren` path). What has *not* been looked at: the hub
   (HS-057+), the changes sheet, and anything phase 2 adds beyond the transcript.

4. **A quiet session stops costing money, on a timer nobody asks for.** `sweeper.rs` reads every `idle`
   session whose `last_active_at` is older than `idle_stop_min` (default 30; **`0` disables it**), applies
   `Trigger::IdleStop` with reason `idle_stop`, logs one `info` line per stop, and spawns the backend's
   `stop`. The tick is a **constant** (5 s), not a config key, so the gate pays a real minute for
   `case_idle_stop`. Only `idle` is a candidate, and the repository's `idle_before` carries that predicate.

5. **A session's spend is a column the event writer adds to, and the runner is what trips the ceiling.**
   `message.end`'s `usage` is added to `sessions.usage_in`/`usage_out` inside `insert_batch`'s own
   transaction (the one that assigns the seq), because the *backend* emits the frame and the runner never
   sees it. `max_session_tokens` (unset = unlimited) is the ceiling; the runner polls the row's
   `usage_in + usage_out` every 100 ms and climbs the ladder with `budget_exceeded` at **or over** it. A
   turn that settles over its ceiling records the reason on its own ending instead (there is nothing left
   to abort), after one extra look a writer tick later. A backend that reports no usage leaves nothing to
   count, and the session screen says so in one sentence (`budgetNotice` in `transcript.ts`) rather than
   showing a zero. The tally survives a resume; that is the spec's "per session".

6. **A session's worktree is real now, and the row records where it is.** `src/worktree.rs` holds spec
   item 1's five host commands as one value (`repository`, `path`, `branch`) with `ensure`, `ahead` and
   `remove`; the Docker backend runs them on `spawn_blocking` and answers
   `Provisioned::Worktree { path, base_commit, created }`; the *provisioner* writes the row, because a
   backend may not (ADR-0019). `ensure` is create-**or-adopt** (a resume re-provisions a worktree that is
   still there) and the row's `base_commit` wins over the branch's head. Teardown treats a missing
   worktree, branch *and* project repository as success, so a delete cannot wedge on ADR-0023's `deleting`
   row. **A step that only adopted is not undone** when a later step fails (`Provisioned::undoable`):
   those commits are the agent's, and deleting them would be data loss dressed as cleanliness.

7. **The step after the worktree is still the one that refuses**, so a Docker-backed create ends `error`
   with `the network step: … (HS-027)` and the provisioner takes the worktree back. That is why
   `case_worktree` asserts the worktree is *gone*, and why the story's "commit in it, see `ahead`
   increment" half is only shown by `tests/worktree.rs` (ADR-0029 records this). The `Session` value a
   later step is handed is the **create's**, so `session.worktree` is still `''` even after the row has
   the path — HS-027's mounts must derive it (`DockerBackend::worktree`) rather than read it.

8. **A project the machine must not mount is refused by the create, with its own 422.**
   `src/worktree.rs` holds `refuse(repository) -> Result<(), Refusal>` (and `Refusal`, `repository_of`,
   `CONFIG_READ`): `.git` must be a directory, no `.gitmodules` and no `.git/modules`, and
   `extensions.worktreeConfig` off (`git config --local --type=bool --get`). `Backends::refuse_project`
   asks the question — the stub answers "no repository", because it mounts nothing — and the create runs
   it on `spawn_blocking` **after** the idempotency lookup and **before** the cap, so a retry still
   answers its row and a refused create never evicts. `resume` asks the same question (HS-026's review
   found the hole). A repository git cannot read is `500 internal` with git's words only in the log, and
   the page prints the refusal's **sentence alone** (`createFailure` in `web/src/transcript.ts`) for the
   three codes. ADR-0030.

9. **Deltas are merged inside one commit, and only inside one commit.** `events::coalesce` runs on the
   writer's batch between `collect` and `commit`: consecutive `message.delta`s merge when the session,
   the message id and the delta's own `text`/`thinking` all match, the merged row keeps the first
   `created_at`, and the run's text is accumulated **outside** the rows and serialised once (a run of one
   keeps its producer's bytes). "The pinned shape" is exact: three keys, no fourth — a payload that is
   not it is passed through untouched and ends the run. **Coalescing is per batch, so two adjacent rows
   of one message and kind are correct when they come from different commits**; a test that forbids them
   at the production 25 ms window fails a correct implementation (that was HS-059's review P1 — the
   live-path test now gives the writer a two-second interval so one commit is a fact). Measured: 5,000
   deltas → 3 rows; the stub's seven scripted deltas → three. ADR-0031.

10. **A Docker run does not start on a machine that cannot carry it.** `src/preflight.rs` checks the
   daemon's `_ping` over `docker_host`, git ≥ 2.38 and `df -Pk <data root>` against `min_free_gb`, and
   `serve --backend docker` refuses **before the database is opened** with one sentence per failure that
   names the fix (all failures at once). `role = "hub"` is exempt — a hub runs no sessions (item 10 §6) —
   and so is `--backend stub`, which the gate and `scripts/check-static.sh` use. `Complaint` is the type
   HS-045's `doctor` will print. `tests/backend_contract.rs` is the conformance suite both backends must
   pass; each Docker story adds its case.

11. **An old message's deltas become one snapshot, and the log is therefore not append-only.**
   `src/compactor.rs` is a task (a pass at startup, then a 60 s tick, `SNAPSHOT_AFTER` = the spec's 24 h
   as a **constant**, not a config key) whose per-session step runs **inside the database mutex the
   transitioner also takes** — so a turn either started before the pass (the row says `running` and the
   pass skips it) or after it. `EventRepository::compact` folds a message's `text` and `thinking` deltas
   into one `message.snapshot` at **the highest seq of the rows it removed**, and `sessions.last_seq` is
   neither read nor bumped: seq is strictly increasing **with gaps**, and the next seq is `last_seq + 1`,
   never `MAX(seq) + 1`. Nothing is broadcast (a connected client already has the deltas; a reconnecting
   one reads the snapshot from its replay). `message.end`, tool frames and turn boundaries are never
   removed, and a delta whose payload is not the pinned three keys stops that session's compaction with
   one `warn` rather than losing text. The window is a constructor argument so a test writes its message
   with an old instant instead of aging a row; the gate case is the one that ages a database. ADR-0032.

12. **A stream no longer replays the whole log, and the client is told where it started.** With no
    `Last-Event-ID`, `EventRepository::truncation_point` scans the message-bearing kinds **backwards** and
    stops one message past `history_window` (a config key, default 50, `0` refused), so the cut is the
    **lowest** seq of the window's oldest message — a block is always whole. The stream then sends a
    `history.truncated` frame carrying `{before_seq, turns_before}` and **no SSE `id`** (it is not a
    stored event, so a browser must not resume past it) and replays from the cut. The cut is computed in
    the stream's read step and **only** when `after == 0`: a reconnecting client already has everything
    below its own seq. `GET /api/sessions/{id}/events/history?before=&limit=` answers the frames below
    that cursor (`seq < before ORDER BY seq DESC LIMIT n`, reversed), `before` is required with a new
    `422 bad_query`, and `limit` is clamped to 1000. The page shows a "Load earlier" button, hidden until
    the frame arrives, one page per click, and folds a page through a fresh `Transcript` at the page's
    `turns_before` into a detached `DocumentFragment` that is `prepend`ed. ADR-0033.
13. **The shared repository gets one verified bundle a day, taken by the create.** `src/backup.rs`
    (`Backups::new/take/prune`) writes `<data>/backups/<project>-<YYYY-MM-DD>.bundle` (UTC) with
    `git bundle create --all`, at most once per project per calendar day — the name *is* the state, and
    it is also the prune's sort key, so no bundle is opened to decide which is oldest. It is written to
    `<name>.bundle.tmp`, checked with `git bundle verify`, then renamed into place, so a file under the
    final name is always one git could read; the last `KEEP = 7` days are kept and the prune runs on the
    already-there path too (a failed prune repairs itself on the next create). `project_backup` is a
    config key that already existed (default **off**); the create takes the bundle **awaited**, after the
    row is inserted and before the provisioner is spawned, on `spawn_blocking`, and **every failure is one
    `warn` line that does not block the session**. The repository is `<projects_root>/<project>` whatever
    backend runs — the bundle is of the host checkout, not of anything a backend knows. ADR-0034.

14. **A session's container is real, limited and on a network of its own — and the mounts hold V1.**
    `src/container.rs` is spec item 1's four mounts as one value (`Mounts`: the worktree at `/work`, the
    repository's `.git` at **its own host path**, and `.git/hooks`/`.git/config` stacked read-only on top of
    it) plus the hardening and the `docker run` vector; `src/docker.rs` is the CLI as a program (`-H`, never
    `DOCKER_HOST`; synchronous, called from `spawn_blocking`). The network step is
    `docker network create --internal agents_<id>` and the container step is `docker run --detach --rm --init
    --name s_<id> --network agents_<id> --cap-drop ALL --security-opt no-new-privileges --workdir /work
    --env HS_SESSION_ID=<id> --cpus/--memory/--pids-limit` + the four mounts. **V1 passes as specified**
    (`ADR-0035`): three writes refused, three git commands working, remount and umount denied, `id -u` 1000.
    Three keys bound it — `container_cpus` 2, `container_memory_mb` 4096, `container_pids` 512 — and all
    three refuse `0` because Docker reads zero as *unlimited*. `Mounts::check` runs **before** every
    `docker run` (§6's landmine: `-v` creates a missing source as an empty root-owned directory).
15. **Every start is `rm -f` then `run`, serialised per session, and `stop`/`resume` are real.** Spec item
    6's first lines, `ADR-0037`: `--rm` removes an exited container a moment **after** `docker stop` has
    answered (measured — a test that read `container_exists` straight after a stop failed one run in
    three), so the removal is a call of its own inside the container step. The lock is `DockerBackend`'s: a
    `tokio` mutex per session id (held across an `await`) over a `std` map that is never held across one;
    mutation-proved — delete it and seven of eight concurrent starts fail with "the name is already in
    use". `stop` is `docker stop --time 10`; `resume` re-creates the network and the container and touches
    no worktree (`ensure` is create-or-adopt, ADR-0029). `SessionBackend::resume` had been uncalled since
    ADR-0022 and now has a body.
16. **The `pi` step still refuses, so a Docker create still ends `error` — one step later than before.**
    The reason is `the pi step: the docker backend does not start the agent yet (HS-031)`. That is the
    observable the gate asserts (`case_docker_container`, `case_docker_resume`): the row reached the
    **agent** step, so the network and container steps both went to a real daemon, and the teardown left no
    container, no network and no worktree. The container runs **detached** with `sleep infinity`
    (`container::IDLE_COMMAND`) until HS-031 attaches pi's stdio, and that placeholder is what lets
    `docker inspect` show the flags before there is an agent to run.

Three things HS-014 and HS-015 settled that later stories inherit:

- **The transcript's event contract** (ADR-0018 + ADR-0019), emitted by the backend:

  | Kind | Payload |
  | --- | --- |
  | `message.delta` | `{"message_id", "kind": "text"\|"thinking", "text"}` |
  | `message.snapshot` | `{"message_id", "text", "thinking"}` (HS-060's compaction is its only producer) |
  | `message.end` | `{"message_id", "usage": {"in", "out"}}` |
  | `tool.start` / `tool.end` | `{"tool_id", "name", "arguments"}` / `{"tool_id", "name", "output"}` |
  | `turn.start` / `turn.end` | `{}` |
  | `turn.steer` | `{"text"}` — **no message id**: one frame, one block, nothing continues it (ADR-0021) |
  | `session.status` | `{"from", "to", "step", "reason"}` (ADR-0016, the transitioner only) |

  A message id keys a transcript block; a delta is one appended text node; a snapshot is the only
  replacing operation and is the last frame for its id; an unknown kind is dropped, not refused. The
  steer frame is the one deliberate exception: its block is created per frame.

- **The backend emits a turn's frames; the server owns every state change.** `start_turn` takes
  `&EventWriter` for that reason, and no backend can reach a `Transitioner`. The prompt's
  `idle → running` is the *handler's* (so the 202 can promise `running`); the way out (`agent_settled`,
  or `unexpected_exit`) is `turn.rs`'s, and an abort's is `turn.rs`'s or the ladder's. Refusals are one
  family: `409 session_<state>` for **all six** states — `session_starting`, `session_idle`,
  `session_running`, `session_stopped`, `session_error`, `session_deleting` — built by
  `ApiError::not_in_state(status, sentence)`, where the **sentence is the caller's** so an abort's
  refusal does not read like a prompt's. Three refusals are outside that family and each has its own
  constructor: `unsaved_work` (a delete whose snapshot failed), `unknown_session` (`not_found`
  with the session's own sentence — a route that exists and a row that does not, HS-019) and
  `cap_without_idle` (`cap_reached` with the create's sentence — the machine is full *and* nothing is
  idle, HS-020).

**Pushing works, through the sandbox's ssh-agent — but the sandbox's own `~/.ssh/config` gets in the
way.** `origin` is `git@github.com:AdamOlsson/holosmith.git`; `bwrap.sh` binds the host's
`SSH_AUTH_SOCK` at `/run/ssh-agent.sock` (the private key never enters the sandbox), so a push signs on
the host. The bound `~/.ssh/config` has a `Host github.com` block with `IdentityFile ~/.ssh/github`, and
that file is **not** in the sandbox, so a plain `git push` fails with
`no such identity: /home/adamolsson/.ssh/github: No such file or directory` / `Permission denied
(publickey)`. It is intermittent — it worked once and failed the next commit — so use the override every
time:

```bash
GIT_SSH_COMMAND='ssh -F /dev/null -o IdentityAgent=/run/ssh-agent.sock \
  -o StrictHostKeyChecking=accept-new -o User=git' git push origin master
```

`-F /dev/null` is the load-bearing part: it stops ssh reading the sandbox's `~/.ssh/config` at all, which
is what names the missing key. (A plain `git push` worked once and then stopped; `-c
core.sshCommand='ssh -o IdentityAgent=…'` was not enough either — the config still won.) `ssh -T
git@github.com` with those options answering `Hi AdamOlsson!` is the way to check the agent before
blaming the remote.

**The reliable form offers ONE key.** The agent holds three, and *which* GitHub accepts changes between
sessions — in this session it was the **third**, offered by `sed -n '3p'`, after the first two were rejected
(`ls-remote` succeeded with it once and failed the next time, so retry the loop rather than trusting one
probe). Loop over the three, offer one at a time, and push as soon as one works:

```bash
for i in 1 2 3; do ssh-add -L | sed -n "${i}p" > /tmp/k.pub
  GIT_SSH_COMMAND="ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityFile=/tmp/k.pub \
    -o IdentityAgent=/run/ssh-agent.sock -o StrictHostKeyChecking=accept-new -o User=git" \
    git push origin master && break; done
```

(The older single-key form, for reference:)

```bash
ssh-add -L | sed -n '2p' > /tmp/pushkey.pub
GIT_SSH_COMMAND='ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityFile=/tmp/pushkey.pub \
  -o IdentityAgent=/run/ssh-agent.sock -o StrictHostKeyChecking=accept-new -o User=git' \
  git push origin master
```

(`-i` names a `.pub` that is in the agent: ssh offers exactly that one and signs with the agent's copy.)
`git ls-remote` with the same options is the cheap way to tell "pushed" from "the agent refused again".
Confirmed on `origin/master`: `b49f2cd` … `0851325`. Push after every story, and if a push fails,
report it — do not fake one.

---

## 2. Environment (already repaired — do not redo)

Everything below lives **outside the repo** and survives. If this is a fresh container, redo it exactly
as written.

```bash
. ~/.pi/toolchains/env.sh      # ALWAYS source this before cargo/rustc
```

It sets `RUSTUP_HOME`, `CARGO_HOME` and `PATH` to:

- rustup 1.99.1, minimal profile + clippy/rustfmt, targets `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-musl`;
- musl.cc cross toolchains for both arches (`x86_64-linux-musl-gcc`, `aarch64-linux-musl-gcc`);
- `~/.pi/toolchains/bin/cc` → `/usr/bin/gcc` (the `cc` crate needs a `cc` on PATH);
- `~/.pi/toolchains/bin/qemu-aarch64-static` (7.2.0) — an **optional dev tool** so the aarch64 artifact
  can actually be *run* here. No repo script may download it; `scripts/check-static.sh` prints one
  `SKIP` line when it is absent and still passes.

Why outside the repo: `/` is a 3.4 GB tmpfs and `~/.pi` is the only large writable ext4 mount.

**Available:** node 24.22.2 / npm 11.19.1 with the npm registry reachable (node 24 strips TypeScript
types natively, which is what `npm test` relies on); `curl`; `python3` (with stdlib `sqlite3` and
`http.client`); `sed`, `grep`, `tr`, `cut`, `cmp`, `find`, `timeout`, `git`; **Playwright's Chromium**
(`~/.cache/ms-playwright`, bound read-only by `bwrap.sh`) and the `playwright` npm package at
`~/.cache/hs-browser/pw/node_modules`. Two quirks, both hit: ESM **ignores `NODE_PATH`**, so a script
imports the package by its own path — `…/playwright/index.mjs` (the `.js` entry is CJS and its default
export does not have `chromium` at the top level); and Chromium must be launched with
`--no-sandbox`, because its own namespace sandbox cannot nest inside ours. A third, learned in HS-020:
**`networkidle` never settles** on the sessions list, because a live row holds an `EventSource` open —
navigate with `domcontentloaded` and wait for the rows.
**Available since this session: a real Docker daemon.** `docker version` shows a 29.9.2 server, `/var/run/docker.sock` exists,
`docker run` works and images pull over the network. `bwrap.sh` binds the host's socket at
`/var/run/docker.sock` (`DOCKER=()` … `--bind "$sock" /var/run/docker.sock`), so a `docker` inside the
sandbox drives the real daemon — that is host root by another name and it is granted deliberately; a
machine without a socket simply gets no binds. **`pi-agent:local` is loaded and is the image every Docker
story uses**: its pi version equals `pins/pi.txt` exactly (HS-031 will assert that in code), git 2.39.5,
node 24, default user `node` (uid **1000**, the host uid) — this is HS-031's agent image in all but name. Everything the Docker chain needs is here, including
network access for `alpine:3` and friends.

**Not available, and not to be invented:** no `sudo`, no `apt`, no `gh`, no ssh key (the agent has one,
§pushing), no Tailscale. There is a **browser** (§1 fact 3), but no *gate* may need one.
**No `awk`** and **no `shellcheck`** on this machine — do not write a script or a gate that needs
either.

### Linear access

There are no Linear tools in the tool list. `list_issues` / `save_issue` / `save_comment` / `get_issue`
are reachable through the same HTTP MCP endpoint `scripts/import-linear.py` uses, so drive it from a
wrapper. Recreate `/tmp/lin.py` if it is gone (`/tmp` is ephemeral):

```python
#!/usr/bin/env python3
# Thin CLI over the Linear MCP server, reusing scripts/import-linear.py plumbing.
#   python3 /tmp/lin.py issues            # id, title, status
#   python3 /tmp/lin.py issue PER-48      # full issue JSON
#   python3 /tmp/lin.py comment PER-48 FILE
#   python3 /tmp/lin.py state PER-48 "In Progress"
#   python3 /tmp/lin.py states            # the team's states
#   python3 /tmp/lin.py call get_issue '{"id":"PER-48"}'
import importlib.util, json, sys
REPO = '/home/adamolsson/projects/holosmith'
spec = importlib.util.spec_from_file_location('imp', REPO + '/scripts/import-linear.py')
imp = importlib.util.module_from_spec(spec); spec.loader.exec_module(imp)
PROJECT = 'Holosmith'
a = sys.argv[1:]
if a[0] == 'issues':
    for i in imp.call('list_issues', {'project': PROJECT, 'limit': 250})['issues']:
        print(i['id'], i['status'], i['title'][:70])
elif a[0] == 'issue':
    print(json.dumps(imp.call('get_issue', {'id': a[1]}), indent=2))
elif a[0] == 'comment':
    print(imp.call('save_comment', {'issueId': a[1], 'body': open(a[2]).read()})['id'])
elif a[0] == 'state':
    imp.call('save_issue', {'id': a[1], 'project': PROJECT, 'state': a[2]})
    print(imp.call('get_issue', {'id': a[1]})['status'])
elif a[0] == 'states':
    print(json.dumps(imp.call('list_issue_statuses', {'team': 'PER'}), indent=2))
elif a[0] == 'call':
    print(json.dumps(imp.call(a[1], json.loads(a[2])), indent=2))
```

Always run it with `PYTHONDONTWRITEBYTECODE=1` and `rm -rf scripts/__pycache__` afterwards, or you will
leave an untracked `scripts/__pycache__/` behind. `list_issues` on this project returns **80 issues**
(not 250); `grep -v ' Done '` is the quickest way to the unfinished ones.

**Two API traps, both hit already:**

- `save_issue` takes `state` (the **name**, e.g. `"In Progress"`). It rejects `stateId`.
- `get_issue` takes only `id`. Passing `project` is a validation error.

The team's states are `In Review`, `Backlog`, `Canceled`, `Duplicate`, `Done`, `Todo`, `In Progress`.
Never invent a state name.

**Token expiry is routine, not a blocker.** When a call prints `The Linear access token has expired.
Re-authenticate 'linear' in pi.`, refresh it in place — the store keeps a refresh token and a client id:

```python
import json, os, time, urllib.request, urllib.parse
p = os.path.expanduser('~/.pi/agent/mcp-auth.json'); key = 'mcp__linear|https://mcp.linear.app/mcp'
d = json.load(open(p)); e = d[key]
body = urllib.parse.urlencode({'grant_type': 'refresh_token',
                               'refresh_token': e['tokens']['refresh_token'],
                               'client_id': e['clientInformation']['client_id']}).encode()
req = urllib.request.Request(e['discovery']['authorizationServerMetadata']['token_endpoint'],
                            data=body, method='POST',
                            headers={'Content-Type': 'application/x-www-form-urlencoded'})
new = json.loads(urllib.request.urlopen(req, timeout=60).read())
e['tokens'] = {**e['tokens'], **new}
e['tokensExpireAt'] = int((time.time() + int(new.get('expires_in', 3600))) * 1000)
json.dump(d, open(p, 'w'), indent=2)
```

If that fails, **stop and report** — that is a `PROMPT.md` stop condition.

---

## 3. The gates

`. ~/.pi/toolchains/env.sh` first. Then:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                     # 255 lib + 35 integration binaries, 600 tests in all (22 in
                               # tests/worktree.rs, 16 tests/events.rs, 10 tests/turns.rs, 10
                               # tests/compaction.rs, 14 delete.rs, 8 idle_stop.rs, 9 backup.rs,
                               # 7 history.rs)
cd web && npm test             # 34 transcript tests (node's own runner)
cd web && npx tsc --noEmit     # not a gate phase, but a web edit that does not typecheck is a bug
scripts/verify.sh              # THE one-command gate since HS-006
```

`scripts/verify.sh` runs six phases, cheapest first: `fmt`, `clippy`, `test`, `build`
(`scripts/build.sh`, which itself runs `build-web.sh` — `npm test` then Vite — and `check-static.sh`
last), `embed` (`scripts/check-embed.sh`), then `serve` — the **shipped musl artifact** booted on
`--port 0` with a temp data dir, running **49 cases plus a CORS sweep (367 assertions)**, then torn down
with `SIGTERM` and an asserted exit 0. Eight cases bring their own boot (`case_token_budget`,
`case_compaction`, `case_history`, `case_backup`, `case_worktree`, `case_worktree_refusal`,
`case_docker_container`, `case_docker_resume`, plus `case_docker_preflight`, which needs no
server at all, and `case_container_mounts`, which needs no server *and* no daemon — it skips, one printed
line, when there is neither a daemon nor an image). It takes ~5 min (both musl targets, the Docker cases
and `case_idle_stop`'s real minute). `--no-build` skips `build` and `embed`; `--keep` keeps the work dir.

A story is not done until all four are green on a clean tree, and then you run the story's own demo
yourself and paste its output.

**Run one build at a time.** A `scripts/build.sh` or `scripts/verify.sh` in the same tree while another
one runs can make `check-static.sh`'s banner wait flake under load (it happened once during HS-013: the
gate reported `FAIL build/failed` while the same command passed a minute later, alone). Serialise them.

**Rebuild after committing.** `git describe` is baked into the artifact and `check-static.sh` fails on a
stale `dist/`, so a commit without a rebuild leaves the gate red. Commit, then `scripts/verify.sh`, then
the demo you paste.

**Rebuild after editing `web/`, before any demo.** The artifact embeds `web/dist`, so a demo run against
an artifact built before the page changed is a demo of the *old* page — and it will look like your fix
did nothing. HS-020 lost a demo cycle exactly this way: the fix to `main.ts` was invisible until
`scripts/build.sh` ran again. `scripts/verify.sh` rebuilds, so "gate green, then demo" is the safe order.

---

## 4. The repository as it stands

```text
Cargo.toml                  workspace: member crates/holosmith, edition 2024, one release profile
Cargo.lock                  COMMITTED, registry-only. check-static.sh fails on a git+ source.
.cargo/config.toml          musl linkers + relocation-model=static (non-PIE) for both targets
pins/pi.txt                 THE ONLY copy of the pinned pi version. Never repeat the literal anywhere.
crates/holosmith/
  build.rs                  git tag (env → git describe → unknown), HOLOSMITH_TARGET, rerun-if-changed
  build_support/web_embed.rs  embed-vs-placeholder decision + placeholder marker
  migrations/0001_initial.sql   schema_version
  migrations/0002_sessions.sql  sessions + two indexes (schema 2)
  migrations/0003_events.sql    events + sessions.last_seq (schema 3)
  src/main.rs               dispatcher only
  src/cli.rs                clap: version, config {validate,show,edit}, serve [--port|--data-dir|--backend]
  src/commands/             version.rs, config.rs, serve.rs
  src/config/               mod.rs (typed Config, KeySpec walk, 26 keys), schema.rs, problem.rs,
                            render.rs, duration.rs, editor.rs
  src/backend/              mod.rs (Step, SessionBackend, Backends::for_run(backend, ContainerSettings),
                            BackendError, TurnEnd, Provisioned), stub.rs (the scripted turn;
                            `ignoring_abort`, `failing_at`, `failing_snapshot`), docker.rs (the worktree,
                            network and container steps are real; `start_container` = `rm -f` then `run`;
                            the per-session start lock; `stop` = `docker stop --time 10`; `resume` = network
                            + container; `Pi` and the turn's five operations still refuse, naming HS-031)
  src/provisioner.rs        the four-step background task and its five session.status events; stops at
                            a step boundary when the row leaves `starting`
  src/turn.rs               the turn runner (HS-015) and the turn timeout (HS-022): touch → the
                            backend's turn → agent_settled + touch, abort_completed (or a `debug` line if
                            the ladder got there first), or unexpected_exit with the reason. Under a
                            deadline the ladder is *spawned* and the turn keeps being awaited;
                            `TURN_TIMEOUT`, `turn::timeout(minutes)`. `TurnEnd` is the backend's answer
  src/ladder.rs             the abort ladder (HS-016): clear_queue → abort → wait out abort_grace →
                            `running → stopped` with the caller's reason → stop. `climb` adds the
                            resume (the abort); `end_turn` returns `Ending::{Asked,Forced}` and does
                            not (the delete, the timeout and the budget). `FORCED_ABORT`
  src/sweeper.rs            the idle stop's task (HS-021): `Sweeper::sweep` (one pass, public for tests),
                            `run` (the tick loop), `spawn`; `IDLE_STOP`, `TICK` (5 s, a constant)
  src/deleter.rs            the delete flow's task (HS-019): revoke_key (a no-op until HS-039) →
                            `backend.delete` → remove the row *and its event log* in one
                            transaction → close the session's streams. `DELETED` is the rung-2 word
  src/container.rs          the session's container (HS-027/028/029, ADR-0035/0036/0037): `Mounts`
                            (spec item 1's four, host path == container path except `/work`),
                            `Mount::argument`/`Mounts::check` (refuses a missing or wrong-kind source
                            *before* `docker run`), `Limits`, `ContainerSettings` (`of(config)`, `at(root)`,
                            `docker()`), `SessionContainer::run_args` (the whole `docker run` vector:
                            `--detach --rm --init --name s_<id> --network agents_<id>`, the hardening, the
                            three limits, `-w /work`, `-e HS_SESSION_ID`), `network_name`, `IDLE_COMMAND`
                            (`sleep infinity` until HS-031), `Unmountable`
  src/docker.rs             the `docker` CLI as a program (HS-028): `Docker{host}`, `run`/`succeeds`,
                            `create_internal_network`/`remove_network`, `remove_container`/`stop_container`
                            (`STOP_SECONDS` = 10), `run_container`, `inspect`, `DockerError`
  src/compactor.rs          compaction's task (HS-060, ADR-0032): `SNAPSHOT_AFTER` (24 h, a constant),
                            `TICK` (60 s), `MESSAGES_PER_PASS` (500), `Compactor::{new, with_window,
                            spawn, run, sweep, fold}`. A pass at startup, then a tick; the per-session
                            step holds the database mutex, so a running turn is skipped by the re-read
  src/backup.rs             the project backup (HS-065, ADR-0034): `Backups::{new, take, prune}`,
                            `BackupError`, `KEEP` (7), `BACKUPS` ("backups"), and the private `day`/
                            `civil_from_days` (UTC, integer-only, no dependency). Sync and std-only, and
                            it logs nothing — the create logs its error
  src/runtime.rs            runtime::Backend {Stub,Docker} — which backend this run uses
  src/http/                 mod.rs, app.rs (Routes/App seam, AppState + provisioner()/turner()/
                            transitioner()/ladder()/deleter()/sweeper()/compactor(), edge layer,
                            is_guard_exempt), error.rs
                            (the one envelope, every code, ApiError::not_in_state / bad_prompt /
                            bad_steer / unsaved_work / unknown_session / cap_without_idle /
                            git_not_directory / submodules_present / worktree_config_enabled),
                            health.rs, guard.rs (the four checks), sessions.rs (POST+GET
                            /api/sessions, GET /api/sessions/{id}, POST …/prompt, …/abort, …/steer,
                            …/resume, DELETE /api/sessions/{id}[?force=true], TURN_STEER, EVICTED,
                            `Slot` + `slot_for` + `evict_one`, `apply_trigger`, and the project's
                            refusals: `refuse_project` + `refusal_error`), streams.rs (the SSE
                            handler)
  src/sessions/             mod.rs (Status, Session, NewSession, RepoError), repository.rs (every
                            `sessions` statement + its SQL-location guard; includes touch,
                            `oldest_idle`, and `remove` — the row and its log in one transaction),
                            state.rs (the transition table, Transitioner::apply / apply_logged, the
                            set_status guard), activity.rs (the one `touch`: lock, warn, carry on)
  src/events/               mod.rs (Event, NewEvent, EventWriter, EventStream, and since HS-059
                            `coalesce` + its private `parse_delta`/`Delta`, which HS-060's fold reads
                            too), writer.rs (`collect` →
                            `coalesce` → `commit`; the failure log names appended events *and* rows),
                            repository.rs (`insert_batch` drops an event whose session has gone;
                            `delete_session_log`; `compact`, the fold into one snapshot; since HS-061
                            `truncation_point`/`turns_before`/`read_before`; and `usage`/`message_id`
                            — the three payload shapes it parses, ADR-0032), stream.rs (replay→live,
                            lag, `Item::Truncated`/`HISTORY_TRUNCATED`, the cut in the read step),
                            registry.rs (open-stream counts)
  src/worktree.rs           the session's worktree (HS-025, ADR-0029): `Worktree::of` (the naming rule),
                            `ensure` (create or adopt, answers `Ensured { base_commit, created }`), `ahead`
                            (`rev-list --count`), `remove` (registered? → remove → branch −D → prune), and
                            `GitError`; and since HS-026 the refusal reads (ADR-0030): `refuse`,
                            `Refusal`, `repository_of`, `CONFIG_READ` (`--local --type=bool`). Sync and
                            std-only; the backend calls it on `spawn_blocking`
  src/preflight.rs          the three startup checks and their sentences (HS-024, ADR-0028):
                            `check`/`check_with` (the `Tools` seam is the test's shim for git/df),
                            `Complaint { check, sentence }`, `socket_sentence`
  src/db.rs                 pragmas + read-back, apply_pending, Database::sessions()/events()
  src/sql.rs                the one SQL statement scanner + the shared source walk (cfg(test) guard)
  src/schema.rs             SCHEMA_VERSION (= 3), include_dir! embed, check_embed, contract_violations
  src/{serve,web,logging,clock,version,pins}.rs
  tests/                    version_cli, config_cli, http_contract, serve_cli, db_migrations,
                            pin_single_source, guards, session_repository, sessions_api, events,
                            streams, provisioning, state_machine, session_list, session_screen,
                            turns, abort, steer, resume, delete, eviction, idle_stop, turn_timeout,
                            token_budget, preflight, backend_contract, worktree, compaction,
                            history, backup, container (V1 + the container's flags + stop/remove/resume +
                            eight concurrent starts; **needs a daemon and `$HOLOSMITH_TEST_IMAGE`,
                            default `pi-agent:local`, and skips otherwise**), support/mod.rs
                            (the shared harness, which also has `repository`/`git`/`head`/
                            `branch_exists`/`worktrees` helpers for a real git repo in a `TempDir`)
web/                        Vite 7 + TypeScript, two screens (`/` and `/sessions/<id>`); untouched by HS-025
                            package-lock COMMITTED; main.ts is the DOM, transcript.ts the renderer's
                            rules (frame parsing, block model, affordances, `stoppedNotice` /
                            `stoppedDetail`, `budgetNotice`, `createFailure`, `parseTruncated`,
                            `setTurnBase`, `historyPath`)
web/test/transcript.test.ts 34 tests over synthetic frames, node's own runner (`npm test`)
scripts/build-web.sh        npm ci + npm test + npm run build, asserts web/dist/index.html
scripts/build.sh            web first, HOLOSMITH_REQUIRE_WEB=1, both musl targets → dist/<triple>/,
                            atomic copy, then check-static.sh. Uses `cargo build --locked`.
scripts/check-static.sh     file/readelf, empty-root serve + cmp against web/dist/index.html, marker
                            gate, app.db assertions, byte grep for the schema SQL, Cargo.lock guard
scripts/check-embed.sh      the three embed cases (present / absent warns / required fails)
scripts/verify.sh           the one-command gate (see §3)
scripts/import-linear.py    the MCP plumbing /tmp/lin.py reuses
docs/adr/0001…0034          read the recent ones before planning anything
docs/build.md               prerequisites, gates, verify.sh, guards, migrations, placeholders
docs/review/PER-33…PER-58, PER-91, PER-92, PER-93, PER-97  each a verbatim review + a `## Resolution` table
docs/stories/               the 80 story slices — the dependency order and definition of done
GLOSSARY.md                 the domain vocabulary. Check it BEFORE inventing a term.
```

### What exists and what does not

- **Working:** `version` (text + `--json`), `config {validate,show,edit}`, `serve` (control listener,
  embedded app, `/api/health`, `POST`/`GET /api/sessions`, `GET /api/sessions/{id}`,
  `POST /api/sessions/{id}/{prompt,abort,steer,resume}`, `DELETE /api/sessions/{id}[?force=true]`,
  `GET /api/sessions/{id}/events`), SQLite with three migrations, the guards, the event writer (which
  coalesces a commit's deltas), the SSE stream, the stub backend and its four-step provisioner, the
  enforced state machine, the abort ladder, the delete flow, the cap with eviction, the create's and the
  resume's three project refusals, the compaction task (which folds a day-old message into one
  `message.snapshot`), the Sessions screen with its create form (which prints a project
  refusal's sentence alone) and the "nothing was deleted" notice, the session screen at
  `/sessions/<id>` with a live transcript and a four-action composer.
- **Not built yet:** the **agent step** (HS-031 — the container runs `sleep infinity` until pi's stdio is
  attached), pi's RPC mapping (HS-032/033/034/035), exit classification and the expected-stop flag (HS-030),
  the `egress` network and its startup self-test (HS-036), the two proxies (HS-037/039), the preview
  listener (HS-041), the WIP snapshot (HS-042), the delete's keep-rules (HS-043), startup reconcile (HS-044),
  `doctor` (HS-045), the phase-1 verification run (HS-046), merge, publish, the hub, releases, the
  stopped-session expiry (HS-064), and any **delete affordance in `web/`** (the API has the endpoint; the
  page has no button).
- **The log is now read in two shapes and written by two producers of two kinds.** The stream replays a
  *window* (`history_window`, default 50) and says so with `history.truncated`; `GET …/events/history`
  answers the frames below that cursor. `events/stream.rs`'s `REPLAY_LIMIT` is still `u32::MAX` — it is
  "everything from the cut", and a second window there would be an invisible truncation. The project
  backup is a second *writer* of files under the data root (`<data>/backups/`), not of the database.
- **Four policies can now stop something and they must not be confused:** the cap evicts (`evicted`,
  inside a create), the sweeper stops a quiet one (`idle_stop`, on its own tick), the turn timeout ends a
  *turn* (`turn_timeout`, from the runner) and the token budget ends a *turn* (`budget_exceeded`, also from
  the runner, on one `fired` shared with the timeout so only one climb happens). All of them go through
  `Transitioner::apply`, and the row moves before the container in every one of them.
- **`runtime::Backend`** has `Stub` and `Docker`; `serve --backend` and the config key `backend` select
  it. `DockerBackend::new(ContainerSettings)` **provisions the worktree, the network and the container, and
  refuses the agent step and the turn's operations** (HS-030 … HS-035 fill them in), and the default
  backend is `docker`
  (ADR-0001: the stub is opt-in), so a default build provisions a session to `error` with a reason
  rather than hanging in `starting`. (Since HS-026 a create for a project that is not a plain checkout is
  refused earlier, with a 422; the demo trick HS-019 used still works for a project that *is* one.) That
  refusal is also how HS-019 provoked a real failed snapshot from the shipped artifact. `scripts/verify.sh` boots with `backend = "stub"` in its
  config, and so does `scripts/check-static.sh`'s empty-root run — a Docker run now refuses at startup on a
  machine that fails the preflight (HS-024), and that sandbox has no daemon, no `git` and no `df`.
- **`AppState`** is `{ config, runtime, database: Arc<std::sync::Mutex<Database>>, events: EventWriter,
  streams: StreamRegistry, backend: Backends }`, with **seven** wiring helpers — `provisioner()`,
  `turner()`, `transitioner()`, `ladder()`, `deleter()`, `sweeper()` (an `Option`, `None` when
  `idle_stop_min` is 0) and `compactor()` (always `Some`). The mutex is an explicit stopgap
  (ADR-0012/0013) — see §6.
- **`sessions::state::Transitioner::apply`** is the only way a session's state changes; `apply_logged`
  is the runners' wrapper (log, never return). A provisioning step report is `from == to` with `step`
  set and is *not* a table row.
- **The create is one lock scope plus a loop.** `create_locked` answers `Slot::{Inserted, Existing,
  Full}`; a `Full` makes `slot_for` evict one idle session (through `apply`, which takes the lock
  itself) and try again. Idempotency is checked first, so a retry never evicts anyone. `evict_one`
  moves the row **before** it spawns the backend's `stop`, because the row is what frees the slot.
- **`config`** has (`31` keys now) `agent_image` (default `agent-image:<binary version>`; this machine's
  demos and the gate set `pi-agent:local`), `container_cpus` (2), `container_memory_mb` (4096),
  `container_pids` (512) — all three **refuse `0`** — plus `abort_grace` (default `10s`, **used** by the ladder), `idle_stop_min`,
  `stopped_ttl_days`, `min_free_gb`, `turn_timeout_min`, `max_session_tokens` and the rest — check
  `config show` before adding a key. The delete flow and **compaction** have **no** config key of their
  own (the 24-hour window is a constant), and `max_live_sessions` (default 3, ≥ 1) is the cap.
- **`POST /resume`** takes the transition, then spawns the provisioner, and **touches `last_active_at`**
  (a resumed session must not be the eviction's first candidate — which is now a real policy, not a
  future one). The cap is checked **before** the transition, and the resume answers `409 cap_reached`
  rather than evicting (ADR-0022, ADR-0024). `SessionBackend::resume` is still uncalled.

---

## 5. The per-issue cycle (what actually worked, twenty-eight times)

Everything is **sequential: one writer at a time**, never two children editing files.

1. **Pick** the issue (ranking in §1) and read its Linear body plus the matching `docs/stories/` slice.
   Note the HEAD SHA — that is the review's fixed point. **Check the issue's title against the story you
   mean**: HS-059 is `PER-91`, and this session's commits for it say `Closes PER-93` because the number
   was assumed rather than read.
2. **Plan.** For **L** stories, use a fresh `delegate` child briefed to run the grilling protocol
   (`~/.agents/skills/grilling/SKILL.md` + `domain-modeling`), which puts **one** round of frontier
   questions to *you* via `contact_supervisor`; answer with `subagent_supervisor({action:"reply", …})`.
   For **S/M** stories, write the plan yourself — it is faster and the analysis is usually already done.
   In either case: state the environment facts, name the files to read, list the decisions already
   settled, and **pre-answer the obvious round-2 questions**. A planner may write only `GLOSSARY.md`
   and a new `docs/adr/*.md`.
3. **Record in Linear before any code exists**: `save_comment` with the plan shape, a
   decisions→precedent table, acceptance restated as testable checks, non-goals, docs touched, the
   demo, and follow-ups; then set the issue `In Progress`.
4. **Implement.** Two patterns, both proven:
   - **S/M with the analysis in hand** (HS-011, HS-012, HS-014, and every story since HS-016): implement
     in-session.
   - **M/L with a lot of new surface** (HS-013's screen, HS-015's turn): hand a fresh `worker` child a
     *written* brief — the plan plus the "how", the ground rules (no commit, no Linear, no new
     dependency, `env.sh` first, don't edit `docs/stories/**`), the exact gate commands, and "budget
     discipline: keep the tree compiling; if you run long, stop green and report which steps remain".
     Launch it async; native completion wakes you. Twice a child found a genuine contradiction in my
     brief — expect that, and verify its claims rather than trusting them.
5. **Verify yourself.** Never trust a child's pasted output: run the gates and the demo. Do this every
   time; it has caught something each story (a wrong test count, a demo-revealed message bug, a panic
   path, and in HS-020 a demo assertion that passed over an already-stopped row).
6. **Review** with a fresh `reviewer` child running `~/.agents/skills/code-review/SKILL.md`, read-only,
   against `<fixed-point>...HEAD` plus untracked files, with the spec axis sourced from the Linear body
   + story slice + spec items. `git add -A` first, then `git diff --cached <fp> > /tmp/holosmith/reviews/
   PER-<n>-diff.txt` so new files appear. **The reviewer child has no shell or write tool**: its claims
   must be checked, and *you* save the review verbatim to `docs/review/PER-<n>.md`. Give it the plan and
   the ADR path too, so its findings are about what those documents do *not* already own.
7. **Fix** in the same session (or with a fresh `worker`), triaging every finding as fixed or
   dismissed-with-citation ("the reviewer misread the helper at …"). Re-run the gates. Append a
   `## Resolution` table to the review file and commit it with the fixes.
8. **Ship**: focused conventional commits naming the issue, the last ending `Closes <IDENT>`. Rebuild
   after committing (and after any `web/` edit — §3). Then `save_comment` with what shipped, the gates,
   verbatim demo output, acceptance item by item, and follow-ups; set the issue `Done`; re-read it to
   confirm.
9. **Refresh this file** at a natural stopping point (counts, HEAD, next issue, the next story's facts).
   It is a cold-start snapshot. The operator asked for it once at the end of a session; refreshing it
   after each story is cheap insurance if the session dies.

Commit messages are prose about *why*, not a restatement of the diff. Look at the last twenty-two for tone.

Working artifacts (`/tmp` is ephemeral; the durable record is Linear + `docs/review/` + `git log`):

```text
/tmp/holosmith/plans/PER-<n>.md           the plan
/tmp/holosmith/plans/PER-<n>-linear.md    the Linear comment body
/tmp/holosmith/plans/PER-<n>-impl-brief.md
/tmp/holosmith/plans/PER-<n>-ship.md      the closing Linear comment body
/tmp/holosmith/reviews/PER-<n>-diff.txt   the diff artifact for the reviewer
/tmp/holosmith/reviews/PER-<n>-review-brief.md
/tmp/holosmith/reviews/PER-<n>-review-verbatim.md
/tmp/holosmith/demo-per-<n>.sh|.py        the demo, with its client half
/tmp/holosmith/web-check*.mjs             the Chromium half, when the story has a screen
```

---

## 6. Landmines (each of these cost real time)

### Build and toolchain

- **The pinned pi version lives in exactly one tracked file, `pins/pi.txt`.** `tests/pin_single_source.rs`
  fails if the literal appears in a second tracked file — including `docs/review/*.md` and this file. A
  commit message is fine; a doc file is not. Every saved review and Resolution table deliberately never
  spells it out.
- **Never export `RUSTFLAGS`.** It silently disables the per-target `rustflags` in `.cargo/config.toml`
  and ships static-PIE. `check-static.sh` asserts the resulting property so a mistake fails loudly.
- **`scripts/build.sh` builds with `cargo build --release --locked`.** Adding a dependency without
  regenerating `Cargo.lock` fails the whole gate. Regenerate, check the lock diff shows only the edge you
  meant, then run the gate.
- **A bare `cargo build` without `web/dist` succeeds with a placeholder page.** `build.sh` sets
  `HOLOSMITH_REQUIRE_WEB=1` and `check-static.sh` greps the artifact for the marker.
- **The artifact embeds `web/dist`, so a `web/` edit needs a rebuild before any demo.** `verify.sh`
  rebuilds; a hand-run demo does not. See §3.
- **No `awk`, no `shellcheck`.** Scripts must use `sed`/`grep`/`tr`/`cut`/`cmp`/`find`/`timeout`. A
  browser exists now, but nothing in `scripts/` may require one.
- **One build at a time**, and **rebuild after every commit** (see §3).
- **node's test runner needs node 22.18+** (native type stripping); this machine has 24.

### Tests

- **Test isolation.** Every test that starts `serve` must give it a temp `data_root` inside its own temp
  dir: this sandbox lets an unprivileged user write `/var/lib`, so a test once created and migrated the
  *real* `/var/lib/holosmith` and still passed.
- **Two `Database::open`s on one path in one process is ADR-0009's measured "database is locked" trap.**
  A shared harness must open exactly one (`OnceLock`); a per-test harness always takes its own
  `TempDir` (`tests/support/mod.rs` carries the pid in the directory name).
- **Same-process SQLite contention bypasses the busy handler.** A two-thread concurrency test is flaky by
  construction; test the lock with a second *process*.
- **A guard test that shares a scratch directory with another guard flakes.** Guards that walk `src/**`
  share `sql::guard`; a directory name comes from its own table word or label.
- **A rule that walks `src/**` must prove it found something.** HS-012's `set_status` guard asserts the
  walk saw both allowed files *before* asserting no offenders — otherwise a broken walk passes green.
- **A startup refusal changes what every default-backend boot does** (HS-024): seven `tests/serve_cli.rs`
  tests and `check-static.sh`'s empty-root run failed the moment the Docker preflight landed. The honest
  fixes are `backend = "stub"` in a shared test config, a fake daemon socket for the two tests that must
  boot Docker, and `--backend stub` in the empty-root run — never a flag that skips the check.
- **A test that asserts only a status is how a wrong code ships.** Assert status **and** `code` **and**
  `content-type`, and prove a new guard fires by removing the behaviour and watching the test go red.
- **The harness's stub is deliberately slow** (`SLOW_STUB_STEP` = 60 s per step) so no test that is not
  about provisioning loses the race against HS-011's background task. Tests that need provisioning to
  finish use `support::with_stub(state, support::fast_stub())` (1 ms). Waiting for a row to reach `idle`
  is `wait_until(...)`, never a `sleep`. That slow stub is also the only way to hold a session in
  `starting` on purpose (HS-020's cap refusal).
- **A row can be ahead of its event.** The runner writes first and the writer commits every 25 ms, so
  "the row says `idle`" does not mean the event is committed. Tests read the log by event count, and the
  stream tests take N frames. HS-018 hit the sharper form: a `prefix` assertion over a log captured from
  a row that had just changed was **vacuously true over zero events** — wait for the event you mean, then
  assert the log is not empty. HS-019 hit it again from the other side: a *before* count taken right
  after `wait_status(idle)` was zero, so "the removal took the log too" would have passed for the wrong
  reason.
- **A background call is not there when the answer is.** HS-020's eviction spawns the backend's `stop`,
  so a test that asserts the backend call must `wait_until` on the recorded call — the **row** is what
  the 202 promised, not the container call.
- **`last_active_at` is whole seconds, so "it moved" is usually the next touch, not yours.** HS-017's
  review proved this by mutation: a steer's `touch` could be deleted and the test stayed green, because
  the turn settled a second later. Push the column into the past first
  (`sessions().touch(id, now - 60)`), then act, then read **immediately** — a `wait_until` reintroduces
  the race. HS-018 changed its stop helper to take a `reason` for the same class of reason: a `None`
  reason made "the resume clears the reason" untestable. HS-020 ages rows the same way to make "least
  recently active" a fact, and ages **two** rows to the same second to prove the `id ASC` tie-break.
- **A prompt turns a race into an expected outcome.** Two prompts can both see `idle`; the loser gets
  `StateError::Illegal` and `apply_prompt` retries once when the row is idle again. Do not "fix" that by
  holding the lock across the read and the apply.
- **A test's claim can be half-strength and still green.** HS-020's review caught one: "rules out the
  lowest id" over **random** ids is a coin toss. If a test claims to rule out a competing rule, make the
  competing rule's answer *deterministic* and wrong — or split it into two tests (which is what the fix
  did). Mutating the rule and watching the test fail is the only proof.

### HTTP and the envelope

- **Every fallible axum extractor answers `text/plain`, not the envelope** — `axum::Json<T>`'s
  rejection, `Bytes`'s (`UnknownBodyError` 400, `LengthLimitError` 413) and `Path<String>`'s (a `%FF`
  segment is a 400). Take the body and the path as `Result<…, Rejection>` and map them yourself.
  `RawQuery` is the exception: its rejection is `Infallible`, so a handler can take it directly.
- **The `Routes`/`App` seam is load-bearing**: `Routes` has no constructor from a bare `Router` and no
  accessor yielding one, `axum::serve` appears once, and `Router::layer` runs in exactly one place —
  `App::new`. `tests/guards.rs` pins the one `.layer(` and the manifest's read-only/mutating kinds.
- **A `GET` route cannot be mutating.** Use `Routes::get`/`head`/`post`/`delete`/…; the manifest records
  the kind. `DELETE` is the only non-`GET`/`POST` verb in the table so far (HS-019's route).
- **Errors are one envelope**, `{"code","message"}` (ADR-0008); codes are constants in `http/error.rs`.
  No test pins a message sentence — but a message must describe *its own* request (HS-015's prompt
  reused the create's sentence until a demo showed it; HS-019's `GET` after a delete said "no such
  endpoint" until a demo showed that too). Two requests may share a code and differ in sentence
  (`cap_reached` for the resume, `cap_without_idle` for the create).
- **`is_guard_exempt` is the only exemption list** (true only for `GET`/`HEAD /api/health`).
- **A mutating request needs `Content-Type: application/json` even with no body.** The `bad_client`
  guard reads the content type as well as `X-Holosmith-Client`, so `POST /abort`, `POST /resume` and
  `DELETE` must send it — a client (or a test) that omits it gets `403 bad_client` and thinks its
  endpoint is missing. The guards check the origin **before** the client header, so a test that omits
  both proves `bad_origin`.
- **In `scripts/verify.sh`, a stream needle can be answered by the *replay of an earlier turn*.** Every
  turn's settle payload is identical, so "wait for `running → idle`" returns the previous turn's frame.
  Use a needle unique to the action (`"reason":"abort_completed"`, `"reason":"evicted"`, the steer's
  text) or poll the row (`wait_for_idle`, `wait_for_gone`), which is what the steer and delete cases do.
- **A `case_*` must own the state it asserts.** HS-020's cap case used to require a session created two
  cases earlier to still be provisioning — a bet on the host's speed — and was moved last, where it
  creates the session it depends on. Before adding a case, ask what it leaves behind *and* what it
  needs from what came before.
- **Banner line order is pinned by tests** (`log`, version line, `config`, `data_root`, `control_listen`,
  `control_origin`, `preview_listen`, `database … schema n`) — and `tests/serve_cli.rs` asserts stderr has
  **exactly eight lines** at default filter. A new `info!` at startup therefore fails a gate test; HS-021
  and HS-022's startup facts are `debug!` for that reason. A *per-event* `info!` line is fine (the gate
  greps for it) because it happens after the banner and only when the thing happens.
- **A `select!` branch that awaits a ladder climb is a bug.** `select!` drops the loser, and a climb
  dropped after rung 2 moved the row leaves the session `stopped` with nobody to resume it — silently,
  because the runner's own ending is the documented `debug` line. `Ladder::spawn` is the entry point
  (HS-022's review caught this).
- **`fetch_sse_until`'s line budget is 800, not 200**, because the shared session's log grows with every
  turn this run takes (four or five turns is ~300 lines of replay). A story that adds turns should re-check
  it; a needle that a *previous* turn's frame can satisfy is still the more common trap (see the stream
  rule above).
- **`--port 0` re-derives `control_origin`** from the bound port after `bind`; the guard compares the
  browser's `Origin` as text, which is what makes it work in tests and demos.
- **`{id}` is axum 0.8's capture syntax**; the spec writes `:id`.

### Data and state

- **A seq counter must never be derived from `MAX(seq)`.** Compaction (HS-060) deletes rows, so the
  counter is the durable `sessions.last_seq`, bumped in the same transaction as the insert.
- **The event writer has one broadcast channel for all sessions**, so a per-session consumer filters by
  `session_id` before comparing seqs — otherwise a foreign seq corrupts the resume point (HS-010's
  blocker).
- **Broadcast strictly after `COMMIT`**, and prove it with a second connection reading inside the
  subscriber; counting rows is vacuous.
- **`id TEXT PRIMARY KEY` is nullable in SQLite.** The invariant is the repository's, not the schema's.
- **`SCHEMA_VERSION` is a literal**; `check_embed` fails the build if it disagrees with the embedded
  files. Bump it in the same commit as the migration. `ALTER TABLE … ADD COLUMN` goes **last**.
- **The repository owns every statement against its table**, enforced by a walk over `src/**`. A new
  *reader* goes through `select()`/`one()`, never a hand-copied column list — HS-020's review caught one
  that had re-spelled all fifteen columns, which would have missed a column a migration added.
- **No state changes outside `sessions::state::Transitioner::apply`.** `set_status` may be *named* only
  in `sessions/repository.rs` and `sessions/state.rs`; a walk over `src/**` enforces it. Integration
  tests outside the crate may still call the raw write (they test the repository).
- **The `session.status` payload is `{from, to, step, reason}`**, four keys, explicit `null`s. A step
  report is `from == to` with `step` set and writes nothing; a transition carries `step: null`.
- **The create's initial state is the table's `none` row**, not a second spelling of `starting`.
- **A backend can emit an event but cannot write the row.** Its `EventWriter` takes free-form
  `kind`/`payload`, so a *forged* `session.status` frame is possible and is exactly the lie ADR-0016
  forbids. ADR-0019 records it as a known hole; closing it means typing the payload (no story yet).
- **A delete takes its row's event log with it** (`sessions::repository::remove`, one transaction), and
  the writer **drops** an event whose session has gone instead of refusing the batch (HS-019). Two
  consequences a test must respect: a deleted session's events cannot be asserted *after* the removal
  (assert the before-count while the row still exists, as `tests/delete.rs` does), and the `deleting`
  event is usually dropped rather than delivered because the removal finishes inside the writer's 25 ms
  tick — so never assert that a watcher saw it.
- **A create evicts outside its own lock scope.** The insert is one scope (`create_locked` →
  `Slot::{Inserted,Existing,Full}`) and an eviction is `Transitioner::apply`'s, which takes the same
  lock; the loop in `slot_for` joins them and terminates because each eviction strictly lowers the live
  count. Do not "simplify" that by writing the status inside the scope.

### Web

- **No new npm dependency, no framework, no router.** `EventSource`, `crypto`, `fetch` are the platform;
  `package-lock.json` is committed; `npm test` is node's own runner.
- **`web/src/main.ts` keeps no state beyond the DOM** (plus the create's pending Idempotency-Key). The
  transcript's rules live in `web/src/transcript.ts` and are tested without a browser — including
  `stoppedNotice`/`stoppedDetail`, because a *sentence* is a rule too.
- **A row's text is written in two places, and they must be one function.** A row is drawn once
  (`sessionRow`) and patched per frame (`applyStatus`); HS-020's blocker was a sentence written only in
  the first. Anything the frame path must show belongs in a pure `transcript.ts` function both call.
  `stoppedNotice` is now **two words wide** (`evicted` "to make room", `idle_stop` "sitting idle"), each
  with its own test and neither shared with `forced_abort`, `deleted` or `turn_timeout` — a stopped row
  for those keeps the dash, and `turn_timeout` is resumed at once anyway so it shows as `idle ·
  turn_timeout`. Adding a third word means a third sentence *and* a test that it is not another's.
- **A delta is one appended text node; a snapshot is `replaceChildren`.** Never `textContent +=` (O(n)
  per delta ⇒ O(n²) per turn), never a document query per frame — blocks are a `Map<message id, nodes>`,
  tools a `Map<tool id, nodes>`.
- **Never `innerHTML` with server data.** Every node is `createElement` + `textContent`.
- **A form inside a page must swallow its own submit** — with no `action` it is a GET of the current path
  and a full reload, which discards the transcript (HS-014's review caught exactly that).
- **The session screen keeps its stream open in every state**, unlike a list row (ADR-0017 closes a dead
  row's source). Both are deliberate; ADR-0018 says why. A delete's stream is closed by the *server*
  (`StreamRegistry::close`), so a row for a deleted session ends and a reconnect is `404`.
- **`turn.steer` is the one frame with no message id** (ADR-0021): its block is created per frame, and
  `order()`/`toolOrder()` do not see it. Do not "fix" that by minting an id without reading ADR-0021's
  price — the log is durable, so two shapes of one kind would live in it forever.
- **The composer is four actions in three states**: prompt on `idle`, steer+abort on `running`, resume on
  `stopped`/`error`, nothing on `starting`/`deleting`. Its POST helper resumes **once** and retries
  **once**, and `abort` passes `mayResume: false` — a resume ends `idle` and abort needs `running`, so a
  retried abort can only fail (HS-018's review found that).
- **The stub's ids carry the process instant** (`msg-<nanos>-<n>`), because a restart that started over
  at `msg-1` would fold a new turn into a previous turn's block against a durable log.
- **`crypto.randomUUID` needs a secure context**; the create form falls back to one `getRandomValues` v4
  and shows the reason if neither exists.
- **A page cannot set `Last-Event-ID`.** `EventSource` resends the last SSE `id` it saw.
- **`case_app_shell` compares `GET /` byte-for-byte with `web/dist/index.html`**, and
  `case_session_screen` does the same for `/sessions/<id>`, so a stale `dist/` fails the gate.

### Config and docs

- **Config output** uses `# <key> unset` rather than omitting an unset optional, and `config show` output
  must re-validate clean. `tests/golden/config-default.toml` pins the exact output, so **a new config
  key means updating the golden file and `KNOWN_KEYS.len()`** (currently 26).
- **`docs/build.md` is a story deliverable, not orchestrator-only.** If a story changes prerequisites or
  gates, the doc is in scope, and its prose must be reflowed (~100 columns), not just appended to.
- **Every story adds at least one `case_*` to `scripts/verify.sh`.** The rule is in bold in
  `docs/build.md`, and the reviewer enforces it. The current gate is 44 cases / 284 assertions, and a
  case that makes its own session must run before the shared one exists or delete it at the end (the
  gate's cap is one live session).
- **Adding an ADR means updating `docs/stories/README.md`**: the "Decisions taken while slicing" count
  (now **thirty-two**) and the bullet list. A reviewer will check the links resolve — three did not,
  once.
- **An ADR that a later story changes gets a one-line "Superseded by ADR-00xx" note**, not a rewrite
  (ADR-0012 and ADR-0013 do this; HS-015 added two to ADR-0018).
- **`GLOSSARY.md` is the vocabulary authority.** Check it before inventing a term. Recent entries to
  respect: **Step**, **Backend**, **Trigger**, **Transition**, **Sessions screen**, **Session screen**,
  **Composer**, **Transcript**, **Delta kind**, **Message id**, **Message snapshot**, **Compaction**,
  **Turn boundary**, **Turn runner**, **Tool call**, **Usage**, **Evict**, **Cap**, **Delete flow**,
  **Unsaved work**. `Status` in code is
  **State** in the glossary; a delta's node is a "kind's node", never a "channel"; a second spelling is
  forbidden, including inside an entry's own prose.
- **`RESUME.md` is the one place this file's own claims can rot.** It contains counts, SHAs and "next
  issue" statements; anything that changes them is a reason to edit it in the same commit.

### Landmines this session added (HS-023, HS-024)

- **A startup check must run before the thing it protects.** HS-024's first draft put the preflight *after*
  `Database::open`, so a refused Docker start had already created and migrated `app.db` while the comment and
  the ADR said "no migration" — a reviewer's P1, and a scripted edit of mine that put the block in the wrong
  place. The gate case now asserts that no `app.db` appeared. **After any scripted move of a block, read the
  resulting order, not just the diff.**
- **A new startup refusal breaks every test and script that boots the default backend.** Seven
  `tests/serve_cli.rs` tests and `scripts/check-static.sh`'s empty-root run failed the moment the preflight
  landed, because the default backend is `docker` and this machine has no daemon. The fixes are the honest
  ones: the shared test config says `backend = "stub"` (with the reason in its doc comment), the two tests
  that must boot a Docker run bring a **fake daemon socket** (one helper), and `check-static.sh` passes
  `--backend stub` because that run is about linking.
- **A check that needs a machine cannot be tested against the machine.** `preflight.rs` takes a `Tools { git,
  df }` seam so a test points it at shims it wrote. THE ALTERNATIVE IS A TRAP: setting `PATH` in a test is
  `unsafe` in edition 2024 (and this crate `deny`s `unsafe`), and it is shared state a concurrent test sees.
- **The writer's 25 ms tick is a race in any test that reads a row right after a turn.** HS-023's own
  `two_turns_add_up` flaked 2 runs in 5 by reading `usage_in` immediately; `wait_for_usage` waits for the
  value. This is the same lesson as HS-017's `last_active_at` and it keeps being relearned.
- **A turn whose last frame is its `message.end` can settle between two polls.** A policy polled at 100 ms
  cannot catch it, so the runner records the reason on the ending it wrote (HS-022's settle-after-the-deadline
  shape) and looks once more one writer tick later — because a tally still in the writer's queue is not a
  tally of nothing.
- **`awk` is still absent** and a `command -v awk` guard caught me using it in a shell one-liner. Use
  `git`/`grep`/`sed`/`cut`/`while read`.

### Landmines HS-025 added

- **`git worktree add` resolves a *relative* path against the repository**, not the caller's directory
  (measured, git 2.43): `git -C repo worktree add ../x` lands inside the repository. Every path in
  `src/worktree.rs` is absolute, and `projects_root` is validated absolute.
- **`git worktree list --porcelain` prints the *resolved* path and does NOT quote it** — a projects root
  reached through a symlink compares equal only after `canonicalize`, and one with a space or non-ASCII
  bytes is printed raw (measured; `core.quotepath` does not change `--porcelain`). `tests/worktree.rs`'s
  adoption test uses a temp dir with a space in its name so this cannot regress.
- **"Already gone" is an exit code, not an exception**: `git worktree remove` on an unregistered path exits
  128 and `git branch -D` of a missing branch exits 1. `remove()` asks `registered()`/`branch_exists()`
  *before* running either, instead of reading git's English.
- **A teardown must succeed when the project repository is missing too** (a delete keeps a row `deleting`
  on a backend failure, ADR-0023). A *directory* that exists but is not registered is left alone — HS-044's.
- **The provisioner only undoes what a provision made.** `Provisioned::undoable` is the rule: an adopted
  worktree's commits survive a later step's failure. Mutating it back to "always undo" makes
  `a_later_step_failure_does_not_take_an_adopted_worktree_back` go red — that test is the guard.
- **`case_worktree` needs a Docker boot on a machine with no daemon.** It answers the preflight's `_ping`
  from a ten-line `python3` unix socket (`fake_docker_ping`), and its python3 guard must stay **above** its
  `stop_serve`: a skipped case that stopped the server first leaves `phase_serve`'s own `stop_serve` with an
  empty pid, and `kill -TERM ""` exits 1 → the gate fails with a message about SIGTERM.
- **`tests/preflight.rs` used to flake about 3 runs in 40** with `git is not runnable (Text file busy
  (os error 26))`: its shims are written and exec'd while other test threads fork, and `exec` of a file any
  process holds open for writing fails. Writing the shim aside and renaming it did **not** help — the
  forked child inherits the write descriptor of the inode that is then exec'd. **Fixed in `1fb7454`**: a
  `one_at_a_time()` mutex held for each whole test body, which works because every test in that binary
  writes shims and nothing else in it forks (8 consecutive runs clean). If another test binary grows the
  same pattern, copy that lock rather than the shim dance.

### Landmines HS-027, HS-028 and HS-029 added (Docker is real now)

- **A path the daemon cannot see is not a mount, and `-v` does not say so.** Docker resolves a bind mount's
  source in the **host's** mount namespace. This sandbox's `/` and `/tmp` are tmpfs, so `docker run -v
  /tmp/x:/work` *creates* `/tmp/x` as an empty root-owned directory on the host and mounts that — no error,
  no warning, and the container comes up with an empty `/work` and a `.git` that is not the repository. The
  first draft of the V1 test lost a cycle to it. Host-visible paths here are the repo (bind-mounted at its
  own path), `~/.pi`, `~/.cache`, `~/.ssh`, `~/.gitconfig` and `~/.local/bin`. **Every Docker test and gate
  case therefore builds its scratch repository under `<repo>/target/…`**, not `mktemp -d`.
- **`--rm` removes an exited container *after* `docker stop` has answered.** Measured: a test that read
  `container_exists` the instant a stop returned failed one run in three. Wait for the removal (a bounded
  poll) or you will chase a phantom. It is also why spec item 6's `rm -f` before every start is load-bearing
  rather than belt-and-braces, and why the container step does the removal itself (ADR-0037).
- **Two starts of one session must not interleave**, or the second `run` fails with "the container name is
  already in use" — which is the *other* start working as designed. The per-session `tokio` lock is in
  `DockerBackend` (`start_lock`), taken by both the container step and `resume`. Deleting it makes
  `two_starts_of_one_session_do_not_race` fail on every run tried (seven of eight starts).
- **The container's image is `agent_image` and must be non-root.** `--user` is deliberately not passed
  (ADR-0035): the image's own user runs the agent, and V1's probe reads `id -u` and fails on `0`. A test or a
  case needs an image with `sh`, `git`, `mount`, `umount` and `sleep`; `pi-agent:local` has all of them and
  its uid **1000** matches the host's, which is what makes the host's files writable from inside.
- **`docker inspect` is the only place a running container's real flags are visible**, and a container that
  failed its next provisioning step has already been removed — so the flags are asserted in
  `tests/container.rs` (which pauses) and the *step order* in the gate's `case_docker_container`/
  `case_docker_resume`. Do not try to catch a container from the gate by polling; it is a race, not a check.
- **A container name is `s_<id>` and a network's is `agents_<id>`, so two tests that share a session id
  collide.** `tests/container.rs` gives each test its own id for exactly that reason (one test's `rm -f`
  deleted the other's container mid-flight).
- **`docker run --detach` returns as soon as the container starts**, so the container's stdout is gone; V1's
  probe strips `--detach` from the production vector and truncates after the image to run its own `sh -c`.
  `--detach` plus `--rm` plus a command that exits means the container vanishes before it can be inspected,
  which is why the placeholder is `sleep infinity`.
- **A case that stops its own server leaves `phase_serve`'s final `stop_serve` with no pid**, and the gate
  fails at `FAIL serve/sigterm`. A case that needs its own boot stops the shared one and **leaves its own
  running**; only the last one is stopped by the phase.
- **A gate case that needs a daemon must guard before it does anything else** and print one `SKIP` line —
  `case_container_mounts`, `case_docker_container` and `case_docker_resume` all do, because a machine
  without Docker is a supported machine (a hub). `HOLOSMITH_TEST_IMAGE` overrides the image;
  `HOLOSMITH_KEEP_SCRATCH=1` keeps `tests/container.rs`'s scratch directory.
- **The gate's `case_worktree` answers the preflight from a `_ping`-only python socket.** Since HS-028 the
  network step really runs and fails against that socket, so the row's reason names **the network step**
  rather than a story number — the assertion was updated, and the fake socket is no longer a stand-in for a
  whole Docker run.

### Reviews

- **Reviews are saved verbatim** and a `## Resolution` section is appended afterwards. Do not edit the
  review body; do not delete a dismissal.
- **The reviewer has no shell.** Its claims must be checked: HS-014 and HS-015 each had one finding the
  reviewer got right and one it got wrong (it read a helper as the wrong function; it called an
  `unreachable!()` path unreachable when a race reached it). HS-019's review read a *stale diff artifact*
  and reported a contradiction that had already been fixed — **regenerate the artifact after any doc edit
  you make while the reviewer is reading**, and say so in the Resolution if it happens.
- It reliably finds one real should-fix per story, and since HS-019 usually one **P1-class** one: HS-019's
  was a refusal that had already ended the turn, HS-020's a sentence written on only one of two DOM
  paths. Both were found in the *acceptance bullet's* own words. Take them seriously.
- **Two of the last four reviews came back `BLOCK`, and each time it was the reviewer, not the plan,
  that was right** — HS-022's was a `select!` the plan itself had argued for, HS-024's a startup check
  ordered *after* the thing it protects. **HS-022 came back `BLOCK`, with a P1 on each axis, and both were
  real.** The spec one was a
  concurrency bug the *plan and the ADR had argued for* (a `select!` around the ladder) — a good plan is
  not a defence. The standards one was a **scripted-edit bug of mine**: a Python insertion into
  `docs/stories/README.md` duplicated a bullet onto one line. After any scripted doc edit, `git diff` the
  file's changed lines and read them; `cargo`/`npm` would never catch it.
- **The reviewer's cheapest useful ask is usually a test that cannot fail.** Two stories running, it
  named one (`each_turn_gets_its_own_clock` built a fresh object per case; `the_bound_is_the_config_key_in_minutes`
  bracketed too loosely) and the fix was to make the wrong design fail. Do that, then mutate the
  behaviour and watch the test go red.
- **Mine its "verification" section.** Its "could not verify without running" list is exactly the
  checklist to run yourself, and its "verified by reading" table is what to quote in the ship comment.

### Demos

- **Demo commands must be bounded.** Background a server with every fd redirected and `timeout`, and kill
  it by its own pid. A backgrounded server keeps the tool call's stdout open and hangs the child.
  `pgrep -f 'holosmith serve'` matches unrelated processes.
- **Launch demos from a script file, not an inline `&` chain** — `cd … && … && binary serve &` puts
  the whole chain in a subshell, so `$!` is not the server's pid and `wait` reports the shell's status.
- **`scripts/verify.sh`'s `fetch` waits for EOF**, which an SSE stream never sends. Use `fetch_stream`
  or `fetch_sse_until`.
- **A demo assertion can pass vacuously.** HS-020's first Chromium check waited for "some row is
  stopped", which an *already stopped* row satisfied instantly, so it proved nothing — it was caught only
  because a stale artifact then made it fail. Key a wait on the id (or the value) you mean, exactly as a
  stream needle must be unique.
- **A `web/` demo needs a rebuild first**, or it is a demo of the previous page (§3).
- **A create's state is not stable in a demo**: the provisioner runs 200 ms/step under `--backend stub`,
  so a `GET /api/sessions` a second later shows `idle`, not `starting`. Assert what the answer *is*.
  That pace is also what makes a real 409 `unsaved_work` demonstrable from the shipped artifact: serve
  with the default `docker` backend, create (the session lands in `error`), and the delete's snapshot is
  refused.

---

### Landmines HS-026 and HS-059 added

- **The gate's cap is one live session, so a case that creates one can evict the run's shared session.**
  `case_event_coalescing` was first placed after `case_prompt_turn`; its create evicted `created_id`
  (`max_live_sessions = 1` in `scripts/verify.sh`'s config) and `case_turn_timeout` then prompted a
  stopped row: `FAIL case_turn_timeout_1/status — expected 202, got 409`. A case that makes its own
  session either runs before the shared one exists or deletes its session as its last assertion.
- **A refusal is read on the request's path, so it must not hold the database lock or block a worker.**
  `refuse_project` is `spawn_blocking`, and the answer is raised *inside* the create's one lock scope
  (as a value passed in) only on the path that would insert — after the idempotency lookup (a retry is
  not a refusal) and before the cap (a refused create must not evict).
- **`git config --get` without a scope reads system + global + local.** A global
  `extensions.worktreeConfig` would have refused every project on the machine; `--local` is the fix and
  the premise was measured (`GIT_CONFIG_GLOBAL` pointing at a temp file: `true` without `--local`, exit 1
  with it). Any future repository read must ask which scope it means.
- **`std::env::set_var` is `unsafe` in edition 2024 and this crate denies `unsafe`**, so a test cannot set
  the process's git config. The `--local` spelling is pinned by a unit test over the `CONFIG_READ`
  constant instead; do not "fix" that by mutating the environment.
- **A repository git cannot read is `500 internal`, not a refusal**, and git's words stay in the log
  (`internal(op, error)` is the repo's one log-and-answer pattern — do not hand-roll `warn!` + `internal()`).
- **Coalescing is per batch, never per log.** Two adjacent rows of one message and kind are correct when
  they came from different commits. HS-059's first live-path test forbade them and passed only because a
  zero-paced stub fit one 25 ms window — the review called it BLOCK, and the fix was to give the test a
  two-second window so one commit is a fact (do not "fix" it by deleting the assertions).
- **The stub paces 200 ms per frame under `serve`, which is eight commit windows**, so the gate cannot
  produce a burst and cannot show a merge. `case_event_coalescing` asserts the *contract* instead
  (never more rows than frames, one message id, no lost text) and says so; a `case_*` that claimed to
  prove the merge would be proving nothing. Two pre-existing assertions (seven deltas in `tests/turns.rs`
  and in `case_prompt_turn`) depend on that pace and now say so.
- **A merged payload is re-serialised, so its spelling becomes canonical.** Accumulate the run outside the
  rows and serialise once; a run of one is never touched. Re-serialising per delta is the quadratic
  re-read ADR-0018 rejects on the renderer side.
- **The Linear issue number is not derivable from the story number.** HS-059 is `PER-91`, not `PER-93`
  (that is HS-061); this session's two code commits say `Closes PER-93` and the correction is recorded in
  `docs/review/PER-91.md`, its Linear comment and PER-93's own. Read the issue's title before writing
  `Closes <IDENT>`.

### Landmines HS-060 added

- **Coalescing makes a row count a race, so a test must wait for the row it means.** A message of three
  text deltas and one thinking delta becomes *two* rows when the writer sees one batch and four when it
  does not — both correct (ADR-0031). `tests/compaction.rs` therefore waits on a predicate (`wait_for_ends`:
  "every message id I wrote has its `message.end`") and never on a count. The first draft waited on
  `log.len() == n` and failed for exactly this reason.
- **A `LIMIT` in a candidate query is applied before the "already done" filter, so a bounded pass stalls.**
  `compact` reads the eligible messages *without* a `LIMIT`, filters out the ones already folded, and only
  then truncates to `MESSAGES_PER_PASS`. With the `LIMIT` in SQL, a session with more old messages than one
  pass may fold would keep re-reading the same oldest ones and never advance. `one_pass_folds_at_most_the_limit`
  proves it by running a second pass.
- **A scan bound is not a delete bound.** `DELTAS` is bounded by `seq < <the newest candidate's
  message.end>`; rows inside that region that belong to a message the pass is not folding are filtered out
  by message id, and the deletes are exactly the rows the fold read. The bound also *narrows* the strictness
  rule to the deltas that could belong to a message being folded — a test that puts a malformed delta after
  the message's `message.end` now proves nothing.
- **The `message.snapshot` a test writes by hand must be `last()`, not `first()`.** `fold.seqs` is built in
  seq order and the snapshot takes the **highest** removed seq; over a message of one delta row (which
  coalescing produces) `first()` and `last()` are the same number and the assertion cannot fail. The seq test
  uses two delta kinds for that reason and asserts `removed.len() >= 2`.
- **A test can age an event without touching the database.** `NewEvent.created_at` is the *producer's*, so
  `tests/compaction.rs` appends its message with an instant two hours old and passes a one-hour window to
  `Compactor::with_window`. Only the gate case, which drives the shipped artifact, has to stop the server and
  rewrite `created_at` with `python3`'s `sqlite3`.
- **A `warn` on a policy path is not noise when the alternative is a silent stall.** `sweep` first discarded
  `Compaction::unreadable`, so a session with a payload the contract does not describe stopped being compacted
  for ever with nothing in the log. The review's P2; the fix is one match arm.
- **The fold holds no `await`, and that is what makes `abort()` safe.** `Compactor::sweep` is an `async fn`
  that takes the std database mutex and does its work synchronously — the same shape
  `Transitioner::apply` has — so `serve`'s `compactor.abort()` can only land at a `yield_now` between
  sessions, never inside a transaction. An `await` inside the fold would let a shutdown roll a pass back
  halfway through its deletes and snapshot.

### Landmines HS-061 and HS-065 added

- **A window makes the "turn" label a lie unless the frame says what it skipped.** The page numbers a
  turn from the `turn.start` frames it has seen, so a replay starting at the 50th message would call its
  first turn "turn 1" while the log says turn 40. `history.truncated` carries `turns_before` for exactly
  that, and it is the one number a client cannot compute. Any future frame that starts a client partway
  through a log needs the same treatment.
- **The cut is a message's first row, so the boundary that introduced it is *above* the cut and is
  replayed.** That is why `turns_before` for a three-turn log with `window = 2` is **2**, not 1, and why
  the gate case's `window = 1` log also counts 2. Both the unit test and `case_history` got this wrong
  first and were fixed by the gate — the arithmetic is not obvious and the code is right.
- **`history.truncated` must not carry an SSE `id`.** It is not a stored event, so a browser that resumed
  after it would resume past a seq the log does not have. `stream_lagged` has the same rule for the same
  reason, and the two frames live together in `events/stream.rs` — not in the stored kinds' block in
  `events/mod.rs`.
- **A page's stability is a property of seq, not of a lock.** `seq < before` with a strictly increasing
  seq means an event committed after the read lands above every row a page can hold. Do not "fix" a page
  by taking a snapshot or a transaction; the only thing that moves a page is compaction removing rows,
  which is ADR-0032's documented gap rule.
- **`before` has no safe default.** "The newest page" is what the stream already gave the client, so a
  missing or unparsable cursor is `422 bad_query` rather than a silent fallback. `limit` is different:
  clamped to 1000, because "give me everything" is a paging mistake, not a malformed request.
- **A test's placement decides what it can prove.** HS-061's stray-delta test put its malformed rows
  *above* the window, where a slot-consuming stray and a scan-ending one both still produced the right
  cut — it could not fail for either bug it named. The strays had to sit *between* the window's messages.
  This is the review's finding and the story's best lesson: **mutate the behaviour and watch the test go
  red** before believing a green one.
- **A scripted append to `docs/stories/README.md` went wrong for the second session running.** HS-022's
  duplicated a bullet onto one line; this session's put the ADR-0033 bullet at the *end of the file*,
  after the "Importing into Linear" section, so the index showed 32 bullets under a count of
  thirty-three. The lesson: after any scripted doc edit, `grep -c '^- \[ADR-'` the file and read the
  changed lines — `cargo`/`npm` will never catch it.
- **A `local a=… b="$a"` is unbound under `set -u`.** `local` is a command: `$a` is expanded before it
  runs. `case_backup` failed the gate with `projects: unbound variable` for exactly that. Two statements.
- **A prune that only runs where a bundle is written stops working for ever after one failure.** HS-065's
  first draft returned early the moment today's bundle existed, so a transient `remove_file` error meant
  a machine would quietly keep every bundle it ever made. The prune now runs on the already-there path
  too — one `read_dir`, once per create — and `take`'s contract says `Err` covers "the retention rule
  could not be applied" as well as "the bundle is not in place".
- **Commit before you review.** HS-065's first `reviewer` child stalled for eleven minutes and had to be
  interrupted: it was reading a working tree that was changing under it, because the fixes it was
  reviewing were uncommitted. The child reads files, not a diff artifact alone, so a moving tree is a
  moving target. Commit, then launch the reviewer.
- **A "verified before it is kept" rule means write aside and rename.** Verifying after the final name
  exists leaves a window where a half-written file sits under a name an operator would trust — and the
  recovery point is the one thing that must not lie. The same shape is worth copying wherever a file must
  never be half-present.

## 7. Decisions taken on the operator's behalf (do not silently undo)

| Decision | Why | Where recorded |
| --- | --- | --- |
| Default branch stays `master` | the repo's own default; renaming would be an invented change | commit history, every run note |
| Ship locally, never fake a push | no credentials exist (`Permission denied (publickey)`, re-tested); reporting the blocker is the honest path | every issue's run note |
| Rust + root Cargo workspace, one binary crate | stories assume Rust | ADR-0004 |
| Static non-PIE musl on both arches, proven with bwrap + qemu, container runs deferred to HS-046 | no Docker daemon; ADR-0003 says a check belongs to the story where changing the design is still cheap | ADR-0005 |
| Config: one typed value, `KeySpec` single table, three-pass validation, `# <key> unset` | "reports every problem at once" is impossible with a single `toml` parse | ADR-0006 |
| axum + the `Routes`/`App` seam, placeholder+marker for the web app, shutdown deadline with streams cut | measured: `axum::serve` rejects a tower-wrapped service; an endless stream blocked shutdown 29 s | ADR-0007 |
| One JSON error envelope | the spec names codes, never bodies | ADR-0008 |
| SQLite: pragmas read back before any transaction, one transaction per start, SQLite's own write lock, `_contract_` naming, older-binary warns | measured: WAL inside a transaction is silently refused; only prepare-time errors carry SQL text | ADR-0009 |
| Guards are one layer inside `App::new`; CORS stripped, not just avoided; `GET` mutability prevented by the route vocabulary | "no CORS header, ever" cannot be a review checklist | ADR-0010 |
| Session id is 16 lowercase hex from `randomblob(8)`, generated by the repository before the insert | the id is pasted into a branch, a container name and two paths; hex is safe in all four and needs no crate | ADR-0011 |
| `status` has no `CHECK`; the six values are a Rust enum | a corrupt row should be a read-time error, and HS-012 adds transitions without rewriting a constraint SQLite cannot drop | ADR-0011 |
| `Arc<std::sync::Mutex<Database>>` in `AppState` as an explicit stopgap | `rusqlite::Connection` is `Send` but not `Sync`; ADR-0009 deferred the sharing decision | ADR-0012 |
| Validation is **shape, not vocabulary** (no model/thinking list) | the spec enumerates neither; inventing a list is a third vocabulary, and HS-031 confirms pi's real values | ADR-0012 |
| The cap refuses with 409 rather than evicting | nothing is evictable until HS-019 can stop and HS-020 can evict | ADR-0012 |
| The mutex stays and the writer task is added on top | ADR-0012's "HS-009 replaces it" conflated a lock with a task | ADR-0013 |
| The SSE heartbeat is axum's `KeepAlive`; `futures-core` is a direct dependency | writing our own comment line would double the heartbeat | ADR-0014 |
| One backend trait with **one method per provisioning step** plus `undo`; an enum `Backends`, not `dyn` | the story's progress reporting *is* step granularity; `async fn` traits are not object-safe | ADR-0015 |
| The stub's per-step delay and the turn's pace are one constructor argument (200 ms under `serve`) | a stub's pacing is not a machine setting | ADR-0015 |
| `backend` **is** a config key (default `docker`), with `--backend` overriding it | the story names both, and ADR-0006 says an override applies to the loaded value | ADR-0015 |
| The transition table is data keyed by `(from, trigger)`; a caller never names a target | a story cannot invent an illegal state, and the write and its event cannot diverge | ADR-0016 |
| "Impossible outside it" is a source walk over `src/**`, not a `pub(crate)` write | the repository's tests live outside the crate and are HS-007's coverage of the write | ADR-0016 |
| The recovery row admits the three **live** states, not `any` | the spec's cell is "server crash", not a state | ADR-0016 |
| `GET /api/sessions` answers `{"sessions":[…],"network_default":…}`; one `EventSource` per visible live session; the page holds only the DOM | one round trip for the form's default; the cap bounds open streams | ADR-0017 |
| The transcript's five event kinds are pinned before their producer; a block is keyed by message id; a delta is one appended text node and a snapshot the only replacement | HS-015 (and HS-032) must emit one shape, not choose their own; re-reading accumulated text per delta is O(n²) per turn | ADR-0018 |
| The renderer's rules live in `web/src/transcript.ts`, tested with node's own runner inside `build-web.sh` | no browser here and no producer at the time; node strips types natively, so it costs no dependency | ADR-0018 |
| The backend emits a turn's frames; the server owns every state change; the prompt's transition is the handler's | the 202 must promise `running`, and a backend must not be able to move a row | ADR-0019 |
| Refusals are `409 session_<state>`, the state's own word — not an umbrella code | the spec names two, HS-017 names a third, and a derived family has no second vocabulary | ADR-0019 |
| `message.end`, `tool.start`, `tool.end` payloads are pinned now; `output_ref` and usage persistence wait | HS-033/HS-034 own their full treatment, and the screen renders them minimally meanwhile | ADR-0019 |
| Story docs in `docs/stories/` are not edited — they are the source the Linear issues were imported from | editing them drifts from Linear; the ADR carries the contract instead | PER-34 plan comment |
| S/M stories are implemented in-session; M/L with new surface goes to a worker child with a written brief | the orchestrator already holds the plan for small ones; a fresh context is better for a large slice | §5, and every Linear note since HS-011 |
| A turn's end is typed (`TurnEnd`), and cancellation is the backend's — no server token, no `select!` (the `select!` half is amended by ADR-0026: one exists, but it cancels nothing and the ladder is spawned) | the agent process is what must stop, and a token would let the server believe it had stopped a turn still running | ADR-0020 |
| Abort climbs two rungs, the row moves before the container is killed, and the ladder's reason is a parameter | the other order leaves a row saying `idle` over a dead container; HS-022 passes `turn_timeout` and HS-023 `budget_exceeded` | ADR-0020 |
| A steer is `turn.steer` with `{"text"}` and **no** message id; the agent is asked before the record is written | a steer is one frame that nothing continues, so an id would have no second use; the record must claim only what was delivered | ADR-0021 |
| No stop endpoint; resume is the only way back, the cap is checked before its transition, and `SessionBackend::resume` stays uncalled | the spec has no stop endpoint, and the policies that stop a session are HS-020/021/044 | ADR-0022 |
| The delete's snapshot is the request's and the removal a task's; the turn is ended by `end_turn` (no resume); the event log goes with the row, so the writer drops an event whose session has gone; idempotency is keyed to the row's state; `force` is the literal `force=true` | only a synchronous snapshot can answer 409 and leave the row intact, and a log nothing can read is dead weight | ADR-0023 |
| The create evicts the least recently active `idle` session outside its own lock scope; only `idle` is a candidate; the row moves before the container and the `stop` is **spawned**; a create with nothing idle answers `cap_reached` with its own sentence while the resume keeps ADR-0022's refusal | an eviction is a state change, and a state change takes the lock; an answer must not wait for container work (spec item 7); the two requests have different next steps | ADR-0024 |
| The idle stop is a sweeper task on a **fixed five-second tick** (a constant, not a config key), its candidates are every `idle` row past `idle_stop_min` from one repository read, each stop is one `apply` plus one spawned best-effort `stop`, and `idle_stop_min = 0` spawns nothing | pacing is not a machine setting (ADR-0015) and the spec's config list is exhaustive (ADR-0006); a policy over rows needs no lock the create holds | ADR-0025 |
| The turn timeout is the **turn runner's own deadline**, it hands the ladder a task of its own rather than a `select!` branch, and both endings record `turn_timeout` | the row keeps no per-turn timestamp (`last_active_at` moves on a steer, so a watcher would let a steered turn run for ever), and a dropped climb leaves a session stopped with nobody to resume it | ADR-0026, amending ADR-0020 |
| The **tally is the event writer's** (inside the seq transaction, so the events repository parses exactly one kind's payload — three since ADR-0032) and the **ceiling is the runner's** (a 100 ms poll, `>=`, first policy to fire owns the reason, a settle over the ceiling records the reason instead) | the backend emits `message.end` and the runner never sees the frame; a subscriber would add lag and a second parser; a policy that must record a reason owns the ending | ADR-0027 |
| The **Docker preflight is a startup refusal** with `Complaint` values reported all at once, `hub` and `stub` exempt, and no new dependency (`df`, `git --version`, a hand-written `_ping`) | the story says "fail at startup instead of mid-turn", item 10 §5 makes the same check `ExecStartPre`, and `unsafe` is denied so `statvfs` was not reachable | ADR-0028 |
| The **worktree's five host commands live in one `src/worktree.rs` value**, the backend runs them on `spawn_blocking`, and the **provisioner** writes the two columns the step reports | item 1 names them as host commands that four later stories read, and ADR-0019 forbids a backend writing a row | ADR-0029 |
| A worktree step that **adopted** a worktree is **not undone** when a later step fails; a delete still removes it | those commits are the agent's work, and deleting them to tidy up after a step that made nothing is data loss; a delete's session is going away and was snapshotted | ADR-0029 |
| The **project's refusals are the create's and the resume's 422** (`git_not_directory`, `submodules_present`, `worktree_config_enabled`), read by `worktree::refuse` through `Backends::refuse_project` on `spawn_blocking`, raised after the key and before the cap, with the stub answering "no repository" | spec item 1 says "refused with 422 at session creation"; the provisioner has no request to answer; a retry must still answer its row and a refused create must not evict | ADR-0030 |
| **Coalescing is the writer's**, a pure `coalesce(batch)` between `collect` and `commit`, merging only consecutive deltas of one session, message id and delta kind, serialising once per run | spec item 15 names the 25 ms commit; ADR-0013 makes the writer the one place a seq is assigned; a per-delta re-serialise is ADR-0018's quadratic mistake | ADR-0031 |
| **`ahead` is a function, not a column and not an endpoint**, and the story's demo is recorded as half-showable | no column exists, HS-061 owns `GET /changes`, a per-row git call in the list endpoint is a second data path; the step after the worktree refuses, so a live worktree cannot be shown from the artifact | ADR-0029 |
| **History truncation is the stream's read step and the window is a key**: with no `Last-Event-ID` a replay starts at the **lowest** seq of the `history_window`-th most recent message (a backwards scan over the message-bearing kinds), announced by a `history.truncated` frame carrying `{before_seq, turns_before}` and **no SSE `id`**; a page is `seq < before ORDER BY seq DESC LIMIT n`, `before` is required with `422 bad_query`, and the page shows a button, not infinite scroll | a block must be whole, so the cut is the message's first row; a reconnecting client already has everything below its own seq; `turns_before` is the one number a client cannot compute, because the frames that would say it are the ones it was not sent; seq is strictly increasing, so a page is stable by construction | ADR-0033 |
| **The project backup is one verified bundle per project per day, taken by the create**: `<data>/backups/<project>-<YYYY-MM-DD>.bundle` (UTC, `--all`), written aside → `git bundle verify` → renamed, the last `KEEP = 7` kept, awaited after the row is inserted and before the provisioner runs, and **every failure is one `warn` that does not block the session**; the repository is `<projects_root>/<project>` whatever backend runs | the name is the state and the prune's sort key; a file under the final name must always be one git could read; the story says "before the first session of each day", so ordering is the requirement, and a broken safety net must not become an outage; the thing an agent can damage is the host checkout | ADR-0034 |
| **The hardened mounts are one value, kept at their host paths, and V1 passes as specified** (so the spec's narrower fallback is not applied), with `--user` deliberately not passed and the image as the key `agent_image` | the worktree's `.git` *file* holds an absolute `gitdir:`, so any other container path points git at nothing mounted; naming a uid would guess a mapping the deploy step owns, and a wrong guess is "permission denied" mid-turn | ADR-0035 |
| **HS-028 fills the network step too**, `--internal` because that is the isolation, with the container **detached** on `sleep infinity` until HS-031 and three integer limit keys that refuse `0` | a container cannot join a network that does not exist, and spec item 6's run line names it; an attached process nobody reads is a container that fills its stdout pipe | ADR-0036 |
| **A start is `rm -f` then `run`, serialised per session by a `tokio` mutex in the backend**; `stop` is `docker stop --time 10` and `resume` re-creates the network and container but no worktree | `--rm` removes an exited container a moment after `docker stop` answers (measured), a container the process lost track of must be replaceable, and interleaved starts fail with a name conflict one caller cannot explain; the worktree step's `ensure` is create-or-adopt | ADR-0037 |
| **Compaction is a background pass inside the database mutex**, one transaction per session, bounded by `MESSAGES_PER_PASS` after the already-folded filter and by the newest candidate's `message.end` on the delta scan; the 24-hour window is the spec's constant and not a key; the snapshot takes the highest removed seq and `sessions.last_seq` never moves; nothing is broadcast | `Transitioner::apply` takes the same mutex, so the skip of a running turn needs no second lock; the spec names 24 hours and its config list is exhaustive; a client already connected has the deltas and a reconnecting one reads the snapshot from its replay | ADR-0032 |

---

## 8. Resume checklist

```bash
cd /home/adamolsson/projects/holosmith
git status --short                          # must be clean
. ~/.pi/toolchains/env.sh
python3 /tmp/lin.py issues | grep -v ' Done ' | head   # next unfinished (recreate /tmp/lin.py, §2)
python3 /tmp/lin.py issue PER-62            # sanity-check Linear is reachable and scoped
docker version                              # the daemon must answer, or every Docker story is a lie
docker image inspect pi-agent:local         # the image the Docker stories run
scripts/verify.sh                           # baseline green (~5-8 min, 49 cases, 367 assertions)
```

Then start the cycle at **`PER-62` (HS-030 — exit classification)**, and after it the rest of the Docker
chain in `HS-###` order: `HS-031` (real pi in the container), `HS-032`–`HS-035` (pi's RPC mapping),
`HS-036` (the `egress` network and the startup self-test), `HS-037`–`HS-040` (the two proxies and their
verification), `HS-041` (preview), `HS-042`–`HS-044` (snapshot, delete keep-rules, reconcile),
`HS-045` (`doctor`), `HS-046` (the phase-1 verification run — the gate for phase 2), then phase 2.

Read §1's sixteen facts first: three of them are the Docker half (mounts and V1, the container and its
limits, the start lock and `resume`), and the rest are the writer's coalescing, the runner's two turn
policies, the worktree step, the create's project refusals, compaction, truncated replay, the daily bundle
and the rendered page.

**`HS-075` (`PER-107`, expand/contract migration discipline) is independent of all of that** and can be
taken at any point — it is the one story whose only blocker is `HS-004`, and §8's own note below carries the
four decisions and three landmines it needs. It is **not shipped**; it was the "next" story before the
daemon appeared and was never started.

**The remaining true blocker is `HS-066` onwards** (phase 2's deploy/hub stories), which wait on
`HS-047`/`HS-056`/`HS-057` and the phase-1 verification run. Nothing in the queue is now blocked on the
machine itself.

### What HS-075 is about, so the first brief is fast

Acceptance, verbatim from `docs/stories/phase-4-self-development.md` (Size M, spec item 10):

- [ ] A release may add but never drop or rename anything the previous release uses; cleanup ships one
      release later.
- [ ] A test asserts the previous release's schema is still a subset of the current one.
- [ ] `doctor` reports schema version and refuses to start when the database is newer than the binary.
- [ ] The rule is documented where a migration author will read it, with an example of the deferred
      cleanup.

**Demo:** a CI check rejects a migration that drops or renames a column the previous release still reads.

**Already in place, so nothing here is rebuilt:** `src/schema.rs` (`SCHEMA_VERSION` = 3, `include_dir!`,
`check_embed`, `contract_violations`); `src/db.rs`'s `apply_pending` and the `_contract_` naming rule
(ADR-0009); `migrations/000{1,2,3}_*.sql`; `tests/db_migrations.rs`; `scripts/check-static.sh`'s byte
grep for the schema SQL; ADR-0009's "older-binary warns" rule.

**The four things to settle before writing code:**

1. **What "the previous release's schema" *is*, mechanically.** There is no released artifact to read here,
   and no git tag to check out. The honest options are (a) a checked-in snapshot of the previous release's
   schema (`docs/schema/previous.sql` or a `migrations/` copy) that the test compares against, or (b) a
   per-migration "drop/rename" scanner over the SQL — a table of forbidden statements — plus a
   subset test against the *current* embedded migrations. (a) is a fact and (b) is a lint; the acceptance
   asks for both ("a test asserts the previous release's schema is still a subset"). Prefer (a) with a
   generated snapshot, and say in the ADR how it is refreshed (a release step, not a story step).
2. **Where the check lives.** "A CI check" — there is no CI here, so it is a `scripts/` check (the gate's
   own `check-migrations.sh`, run from `scripts/verify.sh` as a **phase**, not a `case_*`: it needs no
   server) and a `cargo test`. `scripts/verify.sh`'s phase list is six; adding a seventh is the honest
   shape and `docs/build.md` documents the phases.
3. **`doctor` is HS-045's and is blocked.** The acceptance's third line names it. Two of its three clauses
   are doable now — *report* the schema version (the startup banner already prints `database … schema n`)
   and *refuse to start* when the database is newer than the binary (`apply_pending` must already answer
   something for that; check `db.rs` first, ADR-0009 says "older-binary warns"). Implement the refusal and
   the reporting, and record in the ADR and the Linear comment that `doctor` itself is HS-045's, naming
   the sentence it will print. **Do not invent a `doctor` command here** — HS-045 owns its shape and its
   other checks.
4. **The documentation site.** "Where a migration author will read it" is `docs/build.md`'s migrations
   section (it already exists) plus the ADR; the example of the deferred cleanup should be a real pair of
   migration files or a quoted diff, not prose alone.

**Two traps.** `scripts/check-static.sh` byte-greps the artifact for the schema SQL, so a new migration
changes what that grep sees; and `SCHEMA_VERSION` is a literal that `check_embed` cross-checks against the
embedded files, so a snapshot file added under `migrations/` would be embedded as a migration and break the
build — put the previous-release snapshot somewhere else (`docs/schema/`), not in `migrations/`.

## 9. Reporting

Keep a running ledger in the conversation (issue → plan → implement → review → fix → ship, with SHAs) so
a restart can see what was done from Linear, `docs/review/` and `git log`. The Linear issue is the
durable record; the review file is the durable review; the commit history is the durable change. If you
stop before the backlog is empty, say so explicitly, say which issue is next, and leave the tree clean
and the gates green — a half-finished story is worse than an honest stop.

This session's ledger (three stories shipped — the Docker block is open at last):

| Issue | Story | Commits | Outcome |
| --- | --- | --- | --- |
| PER-59 | HS-027 hardened `.git` mounts and V1 | `3025360`, `b009320` | Done — **V1 measured and passed as specified**, so the spec's narrower fallback is not applied. New `src/container.rs` (the four mounts as one value, the hardening, the `docker run` vector, `Mounts::check`), `agent_image` key, `tests/container.rs` (a real container), `case_container_mounts`. Review: Standards **PASS with findings**, Spec **PASS with findings**, no P1, seven findings all fixed (`docs/review/PER-59-60.md`): dead `docker::absolute` deleted; the teardown's dropped error now a `warn`; the four new keys added to the setter-coverage test; the `0`-refusal tested; two V1 clauses that could pass on a missing `mount`/`id` hardened; a stale doc summary; the sandbox's socket grant documented. Mutation proof: removing the read-only hooks mount makes `hooks_write` succeed. **The one environmental discovery that shaped everything after it:** a path the daemon cannot see is not a mount — `/tmp` is tmpfs here, and `-v` silently creates an empty root-owned directory instead |
| PER-60 | HS-028 container run with limits | `5aae311`, `b009320` | Done — the **network and container steps are real**: `docker network create --internal agents_<id>` and `docker run --detach --rm --init --name s_<id> --network agents_<id> --cap-drop ALL --security-opt no-new-privileges -w /work -e HS_SESSION_ID --cpus/--memory/--pids-limit` + the four mounts. New `src/docker.rs` (the CLI as a program, `-H` not `DOCKER_HOST`), three limit keys that refuse `0`, `tests/container.rs`'s `docker inspect` case, `case_docker_container`, ADR-0036. Detached with `sleep infinity` until HS-031 — recorded, not papered over. The demo is half artifact (a create ends `error` at the **pi** step, so the two earlier steps reached a real daemon, and nothing is left behind) and half test (`docker inspect`: caps, limits, init, auto-remove, network, workdir, four binds, no socket) |
| PER-61 | HS-029 name conflicts, `stop`, `resume`, start lock | `e826539` | Done — `--rm` is **not** enough: the daemon removes an exited container a moment *after* `docker stop` answers (measured; a test failed one run in three until it waited), so a start is `rm -f` then `run`, serialised per session by a `tokio` mutex in the backend. `SessionBackend::resume` finally has a body (network + container, no worktree), `stop` is `docker stop --time 10`, and three new container tests + `case_docker_resume` prove it. **Mutation-proved lock:** delete it and seven of eight concurrent starts fail with "the name is already in use" |

The session before it, for the shape of a ledger entry:

| Issue | Story | Commits | Outcome |
| --- | --- | --- | --- |
| PER-93 | HS-061 history truncation and pagination | `f8c3126`, `315b963`, `0a15876` | Done — Standards **PASS with findings** (2 P2, 2 P3), Spec **PASS with findings** (1 P2, 2 P3); no P1 and no behaviour defect. All five findings real and all fixed: the ADR-0033 bullet had been scripted to the **end** of `docs/stories/README.md` (after the "Importing into Linear" section), `GLOSSARY.md`'s **Last-Event-ID** still promised a replay "from the beginning" 80 lines above the new **Truncated history** entry, five comments/ADRs claimed the same thing (including `budgetNotice`'s, where a settled-turn count from zero would say "no turn has run yet" about a session that has run forty — which is *why* the frame carries `turns_before`), and the spec P2 was the best of the session: `a_delta_that_names_no_message_neither_opens_a_message_nor_ends_the_scan` **could not fail for either bug it named**, because its strays sat above the window where both mutations still gave the right cut; they now sit between the window's messages and **both mutations go red** (`left: 4, right: 3`, and a panic on the `expect`). Two spec P3s dismissed with the reason recorded (the affordance is proved by reading only — no script may require a browser; and "a block is always whole" rests on a producer property now written into ADR-0033). New: `events/repository.rs`'s `truncation_point`/`turns_before`/`read_before`, `Item::Truncated` + `HISTORY_TRUNCATED`, `http/history.rs`, `bad_query`, `history_window` (27th key), `web/src/transcript.ts`'s `parseTruncated`/`setTurnBase`/`historyPath`, the "Load earlier" button, ADR-0033, 7 `tests/history.rs` + 3 repository + 4 node tests, `case_history`. **The gate caught the turn arithmetic twice** (the unit test and the case both said 1 where 2 is right) |
| PER-97 | HS-065 daily project backup | `ea313e4`, `b4d7338`, `d2559bc` | Done — Standards **PASS with findings** (3 P3), Spec **PASS with findings** (2 P2, 2 P3). The two P2s were the valuable ones: the temporary bundle name was a function of project and day while `back_up` runs **outside** the create's lock scope, so two creates on one project could have the second `git bundle create` truncate the file the first was about to rename — and if the second then failed, the name that verified held a file rewritten after the check (now per attempt, `<name>.bundle.<pid>.<n>.tmp`); and "a corrupt bundle is deleted rather than kept" had **no test that could fail**, because git always writes a bundle it can verify, so the delete branch was unreachable (now `Backups::keep` with a junk-file unit test, **mutation-proved**). Also fixed: "is logged" was asserted nowhere (`case_backup` now creates a session for a project with no repository and greps the boot's stderr), `prune` returned a count nobody read, `BackupError` did not implement `Error`, and `Backups::new`'s doc restated its signature instead of the absolute-path invariant. Self-review before the gate had already found two things: the prune only ran where a bundle was written (one transient `remove_file` error and a machine keeps every bundle for ever), and `case_backup` failed on `local projects=… repo="$projects/…"`, **unbound under `set -u`** because `local` is a command. New: `src/backup.rs`, the create's `back_up` on `spawn_blocking`, ADR-0034, 9 `tests/backup.rs` + 1 unit case, `case_backup`, `GLOSSARY.md`'s **Project backup** |
| PER-107 | HS-075 expand/contract migration discipline | — | **Not started.** It is the last unblocked story; §8 carries the four decisions to settle first, the three landmines, and the one acceptance line (`doctor`) that belongs to the blocked HS-045 |

The session before it, for the shape of a ledger entry:

| Issue | Story | Commits | Outcome |
| --- | --- | --- | --- |
| — | (a flake, not a story) | `1fb7454` | **Fixed**: `tests/preflight.rs` failed ~3 runs in 40 with `Text file busy (os error 26)`. `exec` of a file any process holds open for writing fails, and `std::process::Command` forks on another thread's behalf, inheriting the write descriptor of a shim written a moment earlier. A `one_at_a_time()` mutex around each whole test body is the fix, and it works because every test in that binary writes shims and nothing else in it forks. 8 consecutive runs clean; the first baseline gate run of the session had died on it |
| PER-92 | HS-060 message snapshots and compaction | `5c0f960`, `e2a24b6`, `f0ecf8c`, `901b4ef`, `d1cb620` | Done — Standards **PASS with findings** (no P1; 3 P2, 5 P3), Spec **PASS**. The three P2s were all real: ADR-0027/0031 both claimed "the repository parses exactly one kind's payload" and compaction makes it three (amended in both, plus the README parenthetical); `sweep` discarded `Compaction::unreadable`, so a session with a payload the contract does not describe would stall for ever with no log line (one `warn` arm); and the fold read **every** delta row the session had ever held while holding the database mutex (now bounded by the newest candidate's `message.end` — which also *narrows* the strictness rule to the deltas that could belong to a message being folded, and the test moved its bad row accordingly). Also fixed: a self-contradicting `GLOSSARY` **Seq** entry, a `pub(super)` that meant the crate root, and two doc comments. New: `src/compactor.rs`, `EventRepository::compact`, `events::Compaction`, the shared `parse_delta`, ADR-0032, 10 `tests/compaction.rs` cases, `case_compaction`. **Four mutation proofs** (state re-read, `first()` for `last()`, the already-folded filter, the unreadable guard) — and one of them taught the story something: the seq test's message had one delta row because of coalescing, so `first()` and `last()` agreed until the test used two delta kinds |
| PER-58 | HS-026 worktree refusal rules | `18e4695`, `c233ab7` | Done — Standards **PASS with findings** (1 P2, 5 P3), Spec **PASS with findings** (1 P2, 3 P3). The spec P2 was real: **a resume bypassed the guard**, so a project that became unsafe could be re-mounted — `resume` now answers the same three codes. The standards P2 was the repo's one log-and-answer pattern (`internal(op, error)`) re-implemented by hand. Also fixed: `git config` needed **`--local`** (a global `extensions.worktreeConfig` would have refused every project on the machine — measured before the fix), an HTTP case for `Unreadable` → `500`, and the `.gitmodules` rule now argues itself in ADR-0030 (the declaration is checked out into `/work`, so `submodule update --init` writes hooks under the mounted `.git/modules`). New: `worktree::refuse`/`Refusal`, three codes + constructors, `Backends::refuse_project`, `createFailure` on the page, ADR-0030, 12 tests, `case_worktree_refusal`. Also **first: the story rewrote HS-025's behaviour** (a project the machine does not hold is now a 422, not an `error` row), recorded rather than papered over |
| PER-91 | HS-059 event coalescing | `06f506b`, `65f7107`, `5207a97` | Done — Standards **BLOCK** (1 P1, 3 P2, 7 P3), Spec OK with notes. The P1 was real and is the lesson of the session: the live-path test asserted row adjacency **ADR-0031 itself declares false**, and passed only because a zero-paced stub fit one 25 ms window — fixed by giving the test a two-second commit window, not by deleting the assertion. P2s: `coalesce` re-serialised the whole run per delta (quadratic — ADR-0018's own argument), `pub` on an internal helper, and a shape check that would silently drop a fourth key. The **gate case also had to move**: at `max_live_sessions = 1` its create evicted the run's shared session and `case_turn_timeout` failed with `409` where it expected `202`. Measured: 5,000 deltas → 3 rows; the stub's seven → three. New: `events::coalesce`, ADR-0031, 3 `tests/events.rs` cases, a live-path case, 2 node tests, `case_event_coalescing`. **The issue number was wrong** — the commits say `Closes PER-93` (HS-061); `docs/review/PER-91.md` and both Linear issues carry the correction |
| PER-59 | HS-027 hardened `.git` mounts and V1 | — | **Not done: blocked.** No Docker daemon here (`docker` CLI 29.9.2, no `/var/run/docker.sock`), and every acceptance line is a fact about a **container** (six mounts, `--cap-drop ALL`, `no-new-privileges`, the V1 gate). Not implemented against a fake daemon; PER-59 carries the argument and the exact question, and `HS-028 … HS-058` are blocked transitively (their roots — `PER-74`, `PER-79`, `PER-84`, `PER-94`, `PER-100` — carry a pointer) |

The session before it, for the shape of a ledger entry:

| Issue | Story | Commits | Outcome |
| --- | --- | --- | --- |
| PER-55 | HS-023 token budget | `2e902c2`, `fbc0659` | Done — no P1; 1 spec P2 (the two-turn tally the ADR claimed was never tested, and a `SET` mutant stayed green), 2 spec P3s (the no-usage contingency is "counts what was reported", not "disabled"; a glossary overclaim), 8 standards P3s (a mangled doc comment, a helper spliced into another's heading, a variable with no reader, `WRITER_TICK` restated instead of derived, two parsers disagreeing about what a token count is, a middle-man re-export). The gate also caught a flake in the slice: three tests read the tally right after the turn and went red 2 runs in 5 |
| PER-57 | HS-025 worktree provisioning | `7e36bde`, `1fe7f6e`, `ce4dc82` | Done — Standards **PASS with findings** (1 P1, 4 P2/P3), Spec **PASS with findings** (1 P2). The P1 was real: `case_worktree`'s python3 guard sat after its `stop_serve`, so a machine without python3 failed the gate at `FAIL serve/sigterm` while three sentences promised it passed. The valuable one was **the plan's gap, not the code's**: the provisioner undoes every completed step, and the worktree step's undo deletes the worktree *and* `branch -D`s the branch — so a resumed session's adopted worktree (with the agent's commits on it) would be destroyed when the next step refused. Fixed with `Ensured::created` → `Provisioned::undoable`, proved by mutation. Also fixed: the unreachable `not_yet` arm, four doc sentences, and a host-dependent Docker test. New: `src/worktree.rs`, `tests/worktree.rs` (10 cases), `case_worktree`, ADR-0029 |
| PER-56 | HS-024 Docker preflight | `f241ced`, `0b7d50e` | Done — **BLOCK on the spec axis, and it was right**: the preflight ran *after* `Database::open`, so a refused startup had already migrated (my own scripted block move; the gate case now asserts no `app.db`). Plus 5 real P2s (a `hub` was refused, `permission denied` got the `systemctl` advice, `df`'s own words were dropped, HS-046 misattributed as owning ssh in four places, three documents disagreeing about the stub) and 2 standards P2s (a duplicated fake-daemon fixture, two tests that depended on the host's free space). The first gate run also found that `check-static.sh`'s empty-root run needs `--backend stub` |

The session before it, for the shape of a ledger entry:

| Issue | Story | Commits | Outcome |
| --- | --- | --- | --- |
| PER-48 | HS-016 abort ladder | `d034237`, `3d2ecba` | Done — 2 P2s fixed (the grace did not bound rung 1; `Unreported` read as failure), 6 nits |
| PER-49 | HS-017 steer | `0099ca3`, `8ff1d77` | Done — 1 P1 fixed (a `last_active_at` assertion the *settle* satisfied, proved by mutation), 11 notes |
| PER-50 | HS-018 stopped/resume | `dce53c4`, `ff9d907` | Done — 1 P2 fixed (abort retried a dead path, provisioning a session nobody asked for), 16 findings |
| PER-51 | HS-019 delete flow | `3d5c415`, `7c4ea02`, `cd85572`, `db95a3f` | Done — 1 spec P2 fixed (a refused delete of a running session has already ended the turn; now pinned by a test and by the glossary), 4 doc contradictions, 2 nits; also closed PER-41's recorded batch-collateral nit |
| PER-52 | HS-020 cap and eviction | `a3c6825`, `4f37211`, `f67eb6f` | Done — 1 P1 fixed (the "nothing was deleted" sentence was written only where a row is built, so the frame path an eviction arrives by showed a dash; now one function both call sites use, proved in Chromium on the live path), 2 P2 test/gate fixes (a random-id claim; a cap case that bet on the host's speed), 1 P2 duplicate column list, 1 mangled doc comment, 3 stale sentences; the eviction's `stop` is spawned, not awaited |
| PER-53 | HS-021 idle stop | `5da2ae2` | Done — no P1; 5 P2s and 3 nits, all fixed. The two test-claim findings were settled by mutation (the bound now brackets `(90s, 130s]`, and the candidate query's `status = 'idle'` has a test that reads the query); two review claims about the *plan* and the *ADR* were fixed as documentation, and the gate's process-wide `idle_stop_min = 1` is now a written rule for later cases |
| PER-54 | HS-022 turn timeout | `e767757` | Done — **BLOCK on both axes, both real**: 1 spec P1 (the nested `select!` could cancel the climb between rung 2 and the resume, leaving a session stopped forever — now `Ladder::spawn` plus a resume assertion), 1 standards P1 (`docs/stories/README.md` carried the ADR-0025 bullet twice, this session's scripted-edit bug), plus ADR-0020's amendment note, 3 stale-doc fixes, and a "per turn" test that could not fail (now a second fast turn under the same deadline) |
