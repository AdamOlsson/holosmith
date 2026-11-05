# RESUME — orchestrator handoff (cold start, after thirty-six stories, with a real Docker daemon)

You are the **orchestrator for implementing Holosmith**. `PROMPT.md` is the standing operating
procedure and still governs; this file is the state of the world — what exists, what the environment
already has, which decisions are settled, and the landmines that cost real time. Read all of it before
touching anything.

It is a **snapshot, not an archive**: it deliberately carries no per-story history. What each finished
story changed is in Linear (plan, demo, ship note), `docs/review/PER-<n>.md` (the review and its
`## Resolution`) and `git log`; what was decided is in `docs/adr/`. Anything here that a session would not
act on has been cut.

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

Branch **`master`** (this repo has **no `main`**; do not rename it). The tree is clean apart from the
operator's own `log.txt`/`orchestrate.sh`, and every commit is **pushed** (see the note on pushing below).
`HEAD` is `60eaac3`; any commit after it is bookkeeping — this file's own refresh. Confirmed on
`origin/master`: `60eaac3`.

**Linear: 39 Done, 41 Backlog, all in project `Holosmith`.** Done is `HS-001` … `HS-035` (`PER-33` …
`PER-67`), `HS-059` (`PER-91`), `HS-060` (`PER-92`), `HS-061` (`PER-93`) and `HS-065` (`PER-97`).

**The Docker block is gone.** A real daemon answers on this machine (§2), so `HS-027` … `HS-058` are
unblocked and the queue is contiguous from `HS-036` on. Six stories have now been built against a real
`docker run` — the mounts and V1 (`HS-027`), the container with its limits (`HS-028`), the start lock with
`stop`/`resume` (`HS-029`), the expected-stop flag with the exit classifier (`HS-030`), the **agent**
(`HS-031`) and the **turn** (`HS-032`) — and each is proven by a test that runs a container, not by a
document. A Docker create provisions the worktree, the network, the attached container running pi in RPC mode
and pi's ready handshake, and ends **`idle`**; a prompt reaches pi, every record it prints becomes a frame on
the writer, a tool call's output and any image it returned are written to files, the session's spend is
counted in tokens that include the prompt's cached part, and an extension's dialog is cancelled instead of
hanging the turn. The RPC block is **finished**.

Every story from `HS-001` to `HS-035` is Done, plus `HS-059`–`HS-061` and `HS-065` — 39 of 80. The
durable record of each is **Linear** (the plan comment, the verbatim demo output, the ship note), the
review file (`docs/review/PER-<n>.md`, the review verbatim plus its `## Resolution` table) and **`git log`**
(a commit names its issue; the last one ends `Closes PER-<n>`). Decisions are ADRs `0001` … `0043`, indexed
one line each in `docs/stories/README.md`. Nothing about them belongs in this file.

**Next issue: `PER-68` (HS-036 — per-session network and the startup self-test)** — size **M**, the first
story of the network half of the Docker chain, and the one that makes the `--internal` claim *measured* rather
than asserted. §8's own note carries what it needs and what is already there (the per-session network exists
since `HS-028`; what is missing is the `egress` network, the probe, and the startup refusal). Then the rest of
the Docker chain in `HS-###` order: `HS-037`–`HS-040` (the two proxies
and their verification), `HS-041` (preview), `HS-042`–`HS-044` (snapshot, delete keep-rules, reconcile),
`HS-045` (`doctor`), `HS-046` (the phase-1 verification run — the gate for phase 2), then phase 2. What is
still behind something is phase 2 (`HS-047` … `HS-058` are `HS-046`'s, the phase-1 verification run, which is
the story that gates the phase; `HS-066` … `HS-074` sit behind those and behind `HS-049`/`HS-050`), and
`HS-075` (`PER-107`), which is independent of all of it and **still unstarted** — §8's own note carries its
four decisions and three landmines.

Ranking, per `PROMPT.md`: Linear priority (all are `No priority`) → `HS-###` order in `docs/stories/` →
anything already `In Progress` first → skip an issue whose `Blocked by` stories are not `Done`.

### Twenty-three facts a cold start must know

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
   for a climbed stop are `forced_abort`, `deleted`, `evicted`, `idle_stop`, `turn_timeout` and
   `budget_exceeded`; a seventh, `pi_exit`, is not a climb at all but the *clean exit* HS-030 records on a
   `stopped` row (fact 17).
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
3. **The web page has been rendered, including the paths a frame changes.** Playwright's Chromium is
   inside the sandbox (§2): the sessions list and create form, the session screen, the composer's four
   actions, the transcript's blocks and boundaries, a tool call's block **with the links a result's pictures
   are named by** (HS-035: an anchor per picture, never an `<img>`), and the round trips for abort, steer,
   resume, delete, eviction, idle stop, compaction and history paging have all been driven. This is **not** a
   substitute
   for a gate — nothing in `scripts/` may require a browser — but "unverified from source" is not the
   honest answer for a screen that has been looked at. What has **not** been looked at: the hub
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

7. **The step after the container is the one that refuses** (the `pi` step, fact 16), so a Docker-backed
   create ends `error` there and the provisioner takes the container, the network and the worktree back —
   which is why `case_worktree` asserts the worktree is *gone* and why ADR-0029 records the story's
   "commit in it, see `ahead` increment" half as shown only by `tests/worktree.rs`. **The `Session` value a
   later step is handed is the create's**, so `session.worktree` is still `''` even after the row has the
   path: the mounts derive it (`DockerBackend::worktree`) rather than read it, and HS-031's agent step must
   do the same.

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
16. **A Docker create ends `idle`, and a prompt reaches pi (HS-031, HS-032).** The four steps are all real:
    the worktree, the internal network, the attached container and pi's ready handshake. The gate's
    `case_agent_launch` asserts the seeded agent directory, the launch line, `docker inspect`'s command and
    environment, and the exit classifier's third row; `case_docker_resume` asserts a resume reaches `idle`
    again over the same worktree. `case_docker_turn` prompts a real container and asserts pi's *own* refusal
    sentence on the row — a session's network is `--internal` and it holds no provider key until HS-039, so
    pi rejects the `prompt` command before a stream exists. That is a different observable from a hang, and it
    is the most a real container can show until the key proxy exists.

17. **Whether a container stopped because we asked is now a row's bit, and it is decided by a pure
    function.** `sessions.expected_stop INTEGER NOT NULL DEFAULT 0` (schema 4) records "the server asked
    this one to stop"; `src/stop.rs`'s `Stopper` writes it and *then* calls the backend, and it is the only
    path to a `docker stop` — the sweeper, the create's eviction and the ladder's forced rung all use it, so
    there is one place the order can be wrong instead of three. It is **not** in the `Session` JSON (the
    session object is the API's body, pinned to fifteen keys, and phase 1b may not change the API): the
    repository's `set_expected_stop`/`take_expected_stop` read and write it, and the take is a
    compare-and-swap because SQLite's `RETURNING` on an `UPDATE` answers the row *after* the write
    (measured). `src/exit.rs` is the decision: `classify(expected, code, stderr)` with three answers —
    expected → nothing (the row's caller already moved it), code 0 → `Trigger::PiExit`, the table's new row
    to `stopped` with reason `pi_exit`, anything else → `Trigger::UnexpectedExit` to `error` with the last
    five non-empty stderr lines as the reason — and `Exits::record` applies it and appends the stderr to
    `<data>/sessions/<id>/session.log` (`session.log` is stderr plus HS-031's launch line; item 15's "stderr
    only" keeps *tool* output out). **`Exits::record` has no production caller yet**: nothing observes an
    exit until HS-031 attaches pi's stdio, and `--rm` leaves nothing to inspect. **HS-031 inherits one
    ordering constraint from this**: a forced rung resumes *immediately*, so the resume's container step can
    clear the flag before the stopped container's exit is recorded — HS-031 must record that exit before the
    next container is provisioned, or key the flag to the container's incarnation (ADR-0038, cited from
    `provisioner.rs`). The row's reason may now be **multi-line**; `transcript.ts`'s `stateText` renders its
    last line, clipped (`REASON_MAX = 48`), and no badge shows the wall.

18. **The container is the agent, and its stdio is held by the server.** `docker run` is **attached**
    (`-i`, never `--detach`) and its command is `pi --mode rpc --no-session --approve --model <row>
    --thinking <row>` (`agent::COMMAND`); the `sleep infinity` placeholder and `container::IDLE_COMMAND` are
    gone. `src/agent.rs` spawns it as a `tokio::process::Child` (piped stdin/stdout/stderr, `kill_on_drop`),
    splits stdout on **LF only** (`read_until(b'\n')`, one trailing `\r` stripped — never a reader that also
    splits on `U+2028`), routes each `response` to the caller waiting on that command id, drains stderr into a
    **64 KiB tail**, and sends the exit **after** the stdout reader ended so an answer cannot be overtaken by
    the end of the process. `Agent::request(id, json, timeout)` bounds **both** the write and the reply.
    "Ready" is the pi step: a `get_state` command and the `response` that repeats its id, under
    `READY_TIMEOUT` (30 s) — silence and a `success:false` are both failures with a sentence, so `idle` cannot
    be reached without pi answering. The launch is `Agent` + `oneshot::Receiver<agent::Ended>`; the
    **generation** and the registry are the backend's (fact 19).
19. **An exit is the server's to record, and a stale one is not news.** A backend may emit and may not write a
    row (ADR-0019), so the exit crosses on one new seam: `SessionBackend::supervise(&session) ->
    Option<ExitWatcher>`, answered once after a launch, and `Provisioner::run` spawns the task that awaits it
    and calls `Exits::record` — the production caller ADR-0038 promised (ADR-0039). `ProcessEnd::{Exited{code,
    stderr}, NotCurrent}`: the backend's registry holds one entry per session id with a per-launch
    **generation**, the entry is *replaced* by every launch and never removed, and the comparison happens when
    the exit arrives. So the ladder's immediate resume after rung 2 is safe without waiting: an exit that
    arrives before the resume has the flag set (`Expected`), and one that arrives after it is `NotCurrent`.
    **Every removal the server provokes supersedes the launch first** — `remove_container` is the one place that
does it (a bumped generation, not a forgotten entry: the counter is derived from the entry, so forgetting would
restart it at 1 and let an old watcher compare equal), and the container step now goes **through** it, because
`start_container`'s `rm -f` ends the previous agent and that exit arrives before the new launch registers a
generation (HS-032 found this the hard way: a resume classified its own removal as "the container exited 137",
about one run in two). **A delete forgets the launch *before* it removes the container** — not with the expected-stop flag, which
    the delete deliberately does not write: `Exits::record` appends to `session.log` *before* it classifies and
    `SessionLog::append` makes the directory, so a live session's exit would re-create
    `<data>/sessions/<id>` after the delete removed it (HS-031's review, P1). The four other stop policies are
    unchanged.
20. **The agent directory is a fifth mount, and that makes the data root a path the daemon must see.** The
    mounts are now five: item 1's four, then `<data>/sessions/<id>/pi-home` at **its own host path** (not the
    spec's illustrative `/home/agent/.pi/agent` — item 1's host-path identity, and Docker creates a missing
    mount *destination* root-owned for an image whose user this build does not choose). It is created and
    **seeded before** `Mounts::check` (a `-v` of a missing source is an empty root-owned directory), from an
    embedded template (`crates/holosmith/templates/pi-home/settings.json`, `include_str!`) with the row's model
    and thinking in pi's own keys (`defaultModel`, `defaultThinkingLevel`) and no secrets — and **only when
    absent**, because pi writes `auth.json`/`models-store.json` there. `ContainerSettings` therefore carries
    `data_root` and `at(projects_root, data_root)` names both. **Every Docker test and gate case builds its data
    root under `target/`**: a data root the daemon cannot see mounts an empty agent directory and fails the
    handshake with no error anywhere. The launch line (`agent::launch_line`, built from an allow-list of seven
    fields, so nothing is scrubbed) is appended by the backend's own `exit::SessionLog`; stderr reaches the log
    once, from `Exits::record`.

21. **A turn is pi's records mapped to frames by one pure module, and the record seam is where HS-033/035
    extend it.** `src/rpc.rs`'s `Mapping::map(&Value) -> Mapped` is one record in, at most one frame out, and
    `rpc::pump(records, mapping, events)` drives a turn: subscribe **before** the prompt is written, write it,
    append every frame, and end on `agent_settled` (`aborted` → `TurnEnd::Aborted`). The reader
    (`agent::read_records`) routes a `response` to its caller and **publishes** every other record to the
    session's turn through `agent::Slots`, a bounded `mpsc` it *waits* on (backpressure, not loss);
    `Received::{Record, Closed}`, where `Closed` is the reader's own `Slots::close` because the `Agent` value
    in the registry would otherwise keep the channel open for the session's life. Decisions that are easy to
    undo by accident: (a) **the transcript's turn is the operator's prompt**, so `pump` writes `turn.start`
    before the first frame and `turn.end` on the settle, and pi's per-assistant-response
    `turn_start`/`turn_end` are **declined** — mapping them would make "turn 3" mean "third assistant
    response" and tell the page a turn settled while the row was `running`; (b) message ids are minted
    `msg-<first-mint instant>-<counter>`, because pi's messages carry none and the log is durable; (c)
    `text_end`/`thinking_end` content is **not** re-emitted (the deltas already said it, and
    `message.snapshot` has exactly one producer, compaction); (d) `message.end` carries `usage` (optional),
    `stop_reason` and `tool_calls` (the message's tool-call names) — additive keys, ADR-0040 amending
    ADR-0019's table; (e) two counters: **declined** (a documented record with no frame — one `debug` per turn)
    and **unknown** (a type or shape this build has never seen — one `warn` per turn, and that is how a pi
    release that grew a frame gets noticed). (f) **the provider's `toolcall_*` stream is declined and the
    execution records are mapped**: `tool_execution_start` → `tool.start`, `tool_execution_end` → `tool.end`
    (HS-033, ADR-0041), because they carry the complete `args` and the result and pi gives them the same id —
    mapping both would open two blocks for one call; `tool_execution_update` (a partial result) is declined.
    (g) `message.end`'s `usage` counts **what the message cost**: `in` is pi's `input` plus `cacheRead` and
    `cacheWrite`, `out` is `output` (`reasoning` is already inside it — ADR-0042, and `tests/rpc.rs` asserts
    `in + out == totalTokens` on every record of every fixture), and a **zero sum emits no `usage` key at all**,
    so "the provider said nothing" has one spelling. (h) **an extension's dialog is cancelled and its
    fire-and-forget siblings are declined** (HS-035, fact 23): `extension_ui_request` is the last record family
    with a story of its own, and `extension_error` stays declined. The fixtures are **five real turns** under
    `crates/holosmith/tests/fixtures/` (text, thinking, a tool call, an image in a tool result, an extension's
    dialog), captured with the operator's own provider credentials on the default bridge (the image's pi ==
    `pins/pi.txt`), and replayed by `tests/rpc.rs` through the same mapping a container drives; `docs/build.md`
    carries the capture and refresh procedure, and `a_fixture_whose_records_drifted_fails_this_guard` counts
    record types because a mapping assertion cannot see a record that is not there.

22. **A tool call's output is a file, not a payload, and its id is an allow-list.** `src/tool.rs` holds the
    store: `<data>/sessions/<id>/tool/<tool_id>.txt`, `CAP` = 5 MiB of output plus **one** marker line
    (`[truncated: stored N of M bytes]`), the body cut on a character boundary so the file is always UTF-8, and
    `PREVIEW_MAX` = 2000 characters for the frame's `output`. `tool.end` carries `status` (`ok`/`error` from
    pi's `isError` — pi reports no exit code, and the file does **not** keep `structuredContent`, which is where
    a `bash` code lives), `duration_ms`, and `output_ref` **only when the file was written**; the write is
    `pump`'s (which owns the session id and the writer), not the mapping's, exactly as ADR-0040's purity rule
    requires — `Mapped::Tool(ToolEnd)` carries the whole output. `GET /api/sessions/{id}/tool-output/{tool_id}`
    serves it as `text/plain; charset=utf-8` and answers `404 not_found` for an unknown session, an id that
    cannot be a filename and a file that is not there. The id rule is `[A-Za-z0-9_-]{1,128}` (`tool::valid_id`,
    reusing `exit::is_path_segment`), **not** "32 hex": pi's ids are the provider's (`call_<base32>`). `ARGUMENTS_MAX`
    = 2000 characters on `tool.start`, and the rest of the arguments are retained **nowhere** — ADR-0041 says so
    rather than pretending otherwise.
23. **A picture a tool returns is a second file, and a dialog nobody can answer is cancelled** (HS-035,
    ADR-0043). An `image` block of `tool_execution_end.result.content` is stored as
    `<data>/sessions/<id>/tool/<tool_id>-<block index>.<ext>` — the extension from a four-row type table
    (`png`/`jpg`/`gif`/`webp`) with a `.bin` fallback, the base64 decoded by `tool::decode_base64` with the cap
    counted on the **decoded** length *including the trailing group* and checked from the encoded one before
    anything is allocated. `tool.end` gains `images: [{index, mime, output_ref}]` **additively and only when one
    was stored**, served by `GET /api/sessions/{id}/tool-image/{tool_id}/{index}` with the type the *file* has,
    and drawn by the transcript as one link per picture. The store **sweeps** every `<tool_id>-<digits>.<known
    ext>` the new set does not name, so a second result replaces the whole set (the digit requirement is what
    keeps a tool whose id merely begins with this one). An extension's dialog (`select`/`confirm`/`input`/
    `editor`) answers `Mapped::Cancel`, and `pump` writes `{"type":"extension_ui_response","id":…,"cancelled":true}`
    through `Agent::respond` **before** it appends the frame, because pi blocks until the answer arrives; the
    fire-and-forget methods stay declined and an unknown method is the `warn`. The frame is the contract's fifth
    kind, `turn.error` with `{reason, method}` and **no message id** (a steer's shape).

Three things HS-014 and HS-015 settled that later stories inherit:

- **The transcript's event contract** (ADR-0018 + ADR-0019), emitted by the backend:

  | Kind | Payload |
  | --- | --- |
  | `message.delta` | `{"message_id", "kind": "text"\|"thinking", "text"}` |
  | `message.snapshot` | `{"message_id", "text", "thinking"}` (HS-060's compaction is its only producer) |
  | `message.end` | `{"message_id", "usage": {"in", "out"}}` |
  | `tool.start` / `tool.end` | `{"tool_id", "name", "arguments"}` / `{"tool_id", "name", "output", "status", "duration_ms", "output_ref", "images"}` (`status`/`duration_ms`/`output_ref` are HS-033's, `images` is HS-035's and present only when a picture was stored) |
  | `turn.start` / `turn.end` | `{}` |
  | `turn.steer` | `{"text"}` — **no message id**: one frame, one block, nothing continues it (ADR-0021) |
  | `turn.error` | `{"reason", "method"}` — **no message id**, a steer's shape (HS-035) |
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

**pi's own RPC documentation is on this machine**, and HS-031 has now used it (HS-032 – HS-035 will keep
using it):
`~/.pi/agent/install/releases/1.5.0/node_modules/@earendil-works/pi-coding-agent/docs/rpc.md` (how to start
it, the JSONL framing rules — one complete JSON object per line, split on LF only, never a reader that also
splits on U+2028), `rpc-commands.md` (the `prompt`/`steer`/`abort`/`get_state` commands and the `disposition`
field), `json.md` (the event stream: `agent_end`, `agent_settled`, tool and message events),
`rpc-extension-ui.md` (the UI requests HS-035 must auto-cancel) and `environment-variables.md`. The binary in
the image is `/usr/local/bin/pi`, its version already equals `pins/pi.txt`, and `pi --mode rpc --no-session`
is the launch HS-031 is aiming at. `pi --help` in the sandbox is the same version's, so an experiment is
free; nothing in the repo may hard-code the path to those documents.

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
cargo test                     # 310 lib + 38 test binaries, 681 tests in all (22 in
                               # tests/worktree.rs, 16 tests/events.rs, 10 tests/turns.rs, 10
                               # tests/compaction.rs, 14 delete.rs, 8 idle_stop.rs, 9 backup.rs,
                               # 7 history.rs, 10 tests/exit.rs, 5 tests/container.rs, 3 tests/agent.rs,
                               # 12 tests/rpc.rs, 5 tests/tool_output.rs, 10 tests/token_budget.rs)
cd web && npm test             # 41 transcript tests (node's own runner)
cd web && npx tsc --noEmit     # not a gate phase, but a web edit that does not typecheck is a bug
scripts/verify.sh              # THE one-command gate since HS-006
```

`scripts/verify.sh` runs six phases, cheapest first: `fmt`, `clippy`, `test`, `build`
(`scripts/build.sh`, which itself runs `build-web.sh` — `npm test` then Vite — and `check-static.sh`
last), `embed` (`scripts/check-embed.sh`), then `serve` — the **shipped musl artifact** booted on
`--port 0` with a temp data dir, running **54 cases plus a CORS sweep (525 assertions)**, then torn down
with `SIGTERM` and an asserted exit 0. Thirteen cases bring their own boot (`case_token_budget`,
`case_usage_survives_resume`, `case_compaction`, `case_history`, `case_backup`, `case_worktree`,
`case_worktree_refusal`, `case_agent_launch`, `case_docker_resume`, `case_docker_turn`, plus
`case_docker_preflight`, which needs no server at all, `case_container_mounts`, which needs no server *and* no
daemon — it skips, one printed
line, when there is neither a daemon nor an image — and `case_exit_classification`, which reads a row's
`expected_stop` out of `app.db` with `python3` and skips when there is no `python3`). It takes ~5 min (both musl targets, the Docker cases
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
  migrations/0004_expected_stop.sql  sessions.expected_stop (schema 4, HS-030)
  src/main.rs               dispatcher only
  src/cli.rs                clap: version, config {validate,show,edit}, serve [--port|--data-dir|--backend]
  src/commands/             version.rs, config.rs, serve.rs
  src/config/               mod.rs (typed Config, KeySpec walk, 26 keys), schema.rs, problem.rs,
                            render.rs, duration.rs, editor.rs
  src/backend/              mod.rs (Step, SessionBackend, `ProcessEnd`/`ExitWatcher`/`supervise`,
                            Backends::for_run(backend, ContainerSettings),
                            BackendError, TurnEnd, Provisioned), stub.rs (the scripted turn;
                            `ignoring_abort`, `failing_at`, `failing_snapshot`), docker.rs (the worktree,
                            network and container steps are real; `start_container` = `rm -f` then `run`;
                            the per-session start lock; `stop` = `docker stop --time 10`; `resume` = network
                            + container; `Pi` and the turn's five operations still refuse, naming HS-031)
  src/provisioner.rs        the four-step background task and its five session.status events; stops at
                            a step boundary when the row leaves `starting`; after the pi step it spawns the
                            task that awaits the backend's `ExitWatcher` and calls `Exits::record` (HS-031)
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
  src/stop.rs               the one way a container is stopped (HS-030): `Stopper { backend, database }`,
                            `stop` writes `sessions.expected_stop` and *then* calls the backend's `stop`.
                            The sweeper, the create's eviction and the ladder's forced rung all use it, so
                            spec item 6's "before every `docker stop -t 10`" has one home
  src/tool.rs               a tool call's output as a file (HS-033, ADR-0041) and its images as files beside it
                            (HS-035, ADR-0043): `Store::new/store/read/path`, `store_images`/`image`/`read_image`,
                            `preview`/`truncate`/`valid_id`/`floor_boundary`, `decode_base64`, `CAP`/`IMAGE_CAP`
                            (5 MiB), `PREVIEW_MAX`/`ARGUMENTS_MAX` (2000), `MAX_ID` (128), `truncated_marker`,
                            `Stored { preview, reference }`, `Image`/`StoredImage`, `sweep`
  src/exit.rs               exit classification (HS-030, ADR-0038): `Exit::{Expected,Clean,Failed}`, the
                            lifted `SessionLog` (HS-031: the backend appends the launch line), `Exits::for_database`,
                            pure `classify(expected_stop, code, stderr)`, `PI_EXIT`, `STDERR_LINES` (5),
                            `HEADLINE`-free `session_dir`/`LOG_FILE`, and `Exits::record(session, code,
                            stderr)` which takes the flag, applies `PiExit` or `UnexpectedExit`, and appends
                            the stderr to `<data>/sessions/<id>/session.log`. **No production caller yet**:
                            HS-031's attached stdio is the observer
  src/deleter.rs            the delete flow's task (HS-019): revoke_key (a no-op until HS-039) →
                            `backend.delete` → remove the row *and its event log* in one
                            transaction → close the session's streams. `DELETED` is the rung-2 word
  src/agent.rs              the agent process (HS-031, ADR-0039) and its record seam (HS-032): `Agent::launch`
                            (piped stdio, an LF-only JSONL reader that routes `response` records by command id
                            and **publishes** the rest, stderr into a 64 KiB tail, the exit sent after the last
                            record), `records()`/`Slots`/`Received`/`AgentRecords`/`RECORDS_BUFFER` (a bounded
                            channel the reader *waits* on), `request`, `ready`, `Ended`, `AgentError`,
                            `HOME`/`SETTINGS`/`TEMPLATE`/`ENVIRONMENT`/`COMMAND`,
                            `home`/`ensure_home`/`seed`/`launch_line`, `version_of`/`version_check`
                            /`version_sentence`
  templates/pi-home/settings.json  the seed's template, embedded with `include_str!` (no secrets)
  src/container.rs          the session's container (HS-027/028/029, ADR-0035/0036/0037): `Mounts`
                            (spec item 1's four, host path == container path except `/work`),
                            `Mount::argument`/`Mounts::check` (refuses a missing or wrong-kind source
                            *before* `docker run`), `Limits`, `ContainerSettings` (`of(config)`, `at(root)`,
                            `docker()`), `SessionContainer::run_args` (the whole `docker run` vector:
                            `--detach --rm --init --name s_<id> --network agents_<id>`, the hardening, the
                            three limits, `-w /work`, `-e HS_SESSION_ID`), `network_name`, `IDLE_COMMAND`
                            (`sleep infinity` until HS-031), `Unmountable`
  src/docker.rs             the `docker` CLI as a program (HS-028): `Docker{host}`, `run`/`succeeds`,
                            `command_line` (the one call whose stdio a caller keeps: the launch),
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
  src/http/tool_output.rs   one tool's stored output (HS-033): 404 for an unknown session, an id that cannot be
                            a filename and a missing file; `text/plain; charset=utf-8` otherwise
  src/rpc.rs                the mapping (HS-032, ADR-0040; HS-033's tool pair in ADR-0041; HS-034's usage
                            arithmetic in ADR-0042; HS-035's images and dialogs in ADR-0043):
                            `Mapping::{new, map, skipped, unknown}` (one pi record → at most one frame,
                            `Mapped::{Nothing, Frame, Settled, Tool, Cancel}`), `pump(records, mapping, events,
                            store, agent)` (turn.start → frames → turn.end, ending on `agent_settled`), the
                            minted message id, `declined`/`unknown` counters, `ToolEnd`/`result_images`,
                            `Cancel`/`cancelled_reason`, the two additive keys on `message.end` and the two on
                            `tool.end`
  src/http/tool_image.rs    one image a tool returned (HS-035): 404 for an unknown session, an id that cannot
                            be a filename, a non-numeric index and a missing file; the file's own type otherwise
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
  src/schema.rs             SCHEMA_VERSION (= 4), include_dir! embed, check_embed, contract_violations
  src/{serve,web,logging,clock,version,pins}.rs
  tests/                    version_cli, config_cli, http_contract, serve_cli, db_migrations, exit, agent,
                            rpc (the fixture replays and the pump), tool_output (the endpoint and the store's
                            rules at the wire), fixtures/pi-turn-{text,thinking,tool,image,extension-ui}.jsonl,
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
                            `setTurnBase`, `historyPath`, and since HS-030 `stateText`/`headline` — a
                            multi-line reason renders as its last line, clipped at `REASON_MAX`)
web/test/transcript.test.ts 34 tests over synthetic frames, node's own runner (`npm test`)
scripts/build-web.sh        npm ci + npm test + npm run build, asserts web/dist/index.html
scripts/build.sh            web first, HOLOSMITH_REQUIRE_WEB=1, both musl targets → dist/<triple>/,
                            atomic copy, then check-static.sh. Uses `cargo build --locked`.
scripts/check-static.sh     file/readelf, empty-root serve + cmp against web/dist/index.html, marker
                            gate, app.db assertions, byte grep for the schema SQL, Cargo.lock guard
scripts/check-embed.sh      the three embed cases (present / absent warns / required fails)
scripts/verify.sh           the one-command gate (see §3)
scripts/import-linear.py    the MCP plumbing /tmp/lin.py reuses
docs/adr/0001…0042          read the recent ones before planning anything
docs/build.md               prerequisites, gates, verify.sh, guards, migrations, placeholders
docs/review/PER-33…PER-67, PER-91, PER-92, PER-93, PER-97  each a verbatim review + a `## Resolution` table
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
- **The agent is real (HS-031) and so is a turn (HS-032 – HS-034).** A Docker create provisions the worktree,
  the network, the attached container running pi in RPC mode, and pi's ready handshake, and ends **`idle`**;
  the agent's directory and the launch line are under the data root, the exit the attached stdio observes is
  classified, and a resume replaces the container and reaches `idle` again. A **prompt** reaches pi, its
  records become the transcript's frames, a tool call's output is filed, and the session's spend is counted —
  with one gap a real container still shows: pi refuses a `prompt` before a stream exists, because a session's
  network is `--internal` and it holds no provider key until the key proxy (HS-039), so `case_docker_turn`
  asserts pi's own refusal sentence and the streaming evidence is the committed fixtures.
- **Not built yet:** the `egress` network and its startup
  self-test (HS-036), the two proxies (HS-037/039), the preview
  listener (HS-041), the WIP snapshot (HS-042), the delete's keep-rules (HS-043), startup reconcile (HS-044),
  `doctor` (HS-045), the phase-1 verification run (HS-046), merge, publish, the hub, releases, the
  stopped-session expiry (HS-064), and any **delete affordance in `web/`** (the API has the endpoint; the
  page has no button).
- **The log is now read in two shapes and written by two producers of two kinds.** The stream replays a
  *window* (`history_window`, default 50) and says so with `history.truncated`; `GET …/events/history`
  answers the frames below that cursor. `events/stream.rs`'s `REPLAY_LIMIT` is still `u32::MAX` — it is
  "everything from the cut", and a second window there would be an invisible truncation. The project
  backup is a second *writer* of files under the data root (`<data>/backups/`), not of the database.
- **A stop is now recorded as the server's own, and a clean exit has its own state row — observed.**
  `Stopper` writes `expected_stop` before the `docker stop`, and `Exits::record` turns an exit into `stopped`
  (expected), `stopped · pi_exit` (code 0, no flag) or `error` + the last stderr lines. Since HS-031 its
  caller is the task the provisioner spawns after a successful pi step, fed by the backend's `ExitWatcher`
  (ADR-0039). Do not "fix" anything by adding a `docker events` watcher: the attached process *is* the
  observation.
- **Four policies can now stop something and they must not be confused:** the cap evicts (`evicted`,
  inside a create), the sweeper stops a quiet one (`idle_stop`, on its own tick), the turn timeout ends a
  *turn* (`turn_timeout`, from the runner) and the token budget ends a *turn* (`budget_exceeded`, also from
  the runner, on one `fired` shared with the timeout so only one climb happens). All of them go through
  `Transitioner::apply`, and the row moves before the container in every one of them.
- **`runtime::Backend`** has `Stub` and `Docker`; `serve --backend` and the config key `backend` select
  it. `DockerBackend::new(ContainerSettings)` provisions all four steps and runs a turn's five operations;
  what is left of the Docker chain is the network story (HS-036 onwards), and the default backend is `docker`
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

## 5. The per-issue cycle (what actually worked)

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

Commit messages are prose about *why*, not a restatement of the diff. Look at the last few for tone.

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
  `docs/build.md`, and the reviewer enforces it. The current gate is 54 cases / 525 assertions, and a
  case that makes its own session must run before the shared one exists or delete it at the end (the
  gate's cap is one live session).
- **Adding an ADR means updating `docs/stories/README.md`**: the "Decisions taken while slicing" count
  (now **forty-three**) and the bullet list. A reviewer will check the links resolve — three did not,
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

### Landmines: startup checks, refusals and the writer's tick (HS-023, HS-024)

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
- **`awk` is absent** (§2) — use `git`/`grep`/`sed`/`cut`/`while read`, and check with `command -v` before
  writing a one-liner from muscle memory.

### Landmines: worktrees (HS-025)

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

### Landmines: Docker — mounts, containers, starts (HS-027 – HS-029)

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

### Landmines: project refusals and coalescing (HS-026, HS-059)

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

### Landmines: compaction (HS-060)

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

### Landmines: history windows and backup (HS-061, HS-065)

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
- **A scripted doc edit has now gone wrong three times** (a bullet duplicated onto one line; an ADR bullet
  appended after the "Importing into Linear" section; the same page's count drifting from its bullet list).
  After any scripted edit of a Markdown file, `grep -c '^- \[ADR-'` it and read the changed lines — `cargo`
  and `npm` never will.
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

### Landmines: the expected-stop flag and the exit classifier (HS-030)

- **SQLite's `RETURNING` on an `UPDATE` answers the row *after* the write.** `UPDATE … SET expected_stop = 0
  … RETURNING expected_stop` therefore reports `0` for a flag that was `1`, so "take the flag" cannot be
  spelled that way; PostgreSQL's `OLD` has no SQLite spelling either. The take is a compare-and-swap
  (`… WHERE id = ?1 AND expected_stop = 1`, and `execute`'s changed-row count is the answer). Measured after
  a test failed with `Some(false)` for a value the same connection had just set.
- **A test named for a function must call it.** HS-030's first draft had
  `the_classifier_is_the_one_exit_record_uses` asserting three `classify` results and never touching
  `record`; the review found it. The duplicate is the smell — the pure cells already live in the module's
  unit tests.
- **An assertion that cannot distinguish two arms is not an assertion.** `an_expected_exit_…` drove a row
  that was already `stopped`, where *both* classifier answers are no-ops (the table has no
  `unexpected_exit` row out of `stopped`) — so it passed with `classify` ignoring the flag entirely. The
  discriminating case must start from a **live** row. Mutation-prove every claim about an arm.
- **A spawned task's write is not there when the row is.** The eviction's stop — and so the flag write — runs
  after the row moved, so `case_exit_classification` polls for the flag (50 × 100 ms) instead of reading it
  once; the Rust test waits for `Call::Stop`. Same lesson as HS-020's backend call.
- **`status_reason` is rendered in a badge five times over, so a new *shape* of reason is a rendering
  change.** HS-030's failure reason is the last five stderr lines; `transcript.ts`'s `stateText` is now the
  one place that decides how much of it shows (its last line, clipped). The plan said "no UI change" and was
  wrong — a row's *text* is still a rule.
- **A `Stopper` cannot prove its own ordering.** Write-then-call is two lines, and no test outside them can
  see which came first (the swap mutation stays green). ADR-0038 says so rather than pretending; do not
  "fix" it with a fake backend seam.
- **`doc` edits must not be made with `git checkout` to undo a mutation.** HS-030's session reverted
  `sessions/state.rs`'s uncommitted work that way (it was restored by hand). Copy the file aside
  (`cp x /tmp/x.bak`) before a mutation, and never `git checkout` a file with uncommitted changes.

### Landmines: the agent launch (HS-031)

- **An attached container dies with the runtime that spawned it.** `tests/container.rs` used to make a
  throwaway `tokio` runtime per call; with HS-031 that killed the launch's child (and, through `--rm`, the
  container) the moment the step returned, and `docker inspect` said "no such object". Every such test now
  holds **one** runtime for its whole body.
- **The container appears *seconds* after the container step returns.** The step's success means the process
  was spawned, not that the daemon has created the container (measured here at several seconds). Anything that
  inspects one waits for it (`wait_running` in `tests/container.rs` / `tests/agent.rs`); the gate waits for the
  row to say `idle`, which is after pi answered. Do not read the flags straight after the step.
- **A killed `docker` CLI does not stop its container.** `kill_on_drop` kills the CLI; the container keeps
  running with nobody talking to it. That is HS-044's reconcile, and the next start's `rm -f` covers it
  meanwhile.
- **The data root is now a path the daemon must see**, because `<data>/sessions/<id>/pi-home` is a mount.
  Every Docker test and gate case builds its data root under `target/`; a `/tmp` data root mounts an empty
  root-owned directory and fails the handshake with **no error anywhere** (ADR-0035's measurement, second
  root).
- **The pi-home mount's *destination* is its own host path**, not the spec's `/home/agent/.pi/agent`: Docker
  creates a missing destination root-owned, and the image's user is the image's choice (uid 1000 here), so a
  destination under the image's home would hand pi a directory it cannot write.
- **`kill_on_drop` + `--init` + `-i` means the command vector must not be truncated by a test that then
  expects a container**: `tests/container.rs`'s planted leftover adds `--detach` and cuts after the image, and
  V1 does the same to run its own probe.
- **Two tests that share a session id share a container *and* a network.** `tests/agent.rs` gives each test its
  own id — the first draft had both use one, and the network step failed with "already exists" (HS-027's
  lesson, again).
- **`get_commands` is how project trust is observable**, and it needs a project `.pi/` resource in the
  worktree. pi writes `auth.json`/`models-store.json` into the agent directory on first start, which is why the
  seed is written only when absent.
- **A test's fake agent should read one line before it answers.** The first version of `agent.rs`'s unit tests
  wrote its records and exited immediately, so the handshake's write hit `Broken pipe` and the failure looked
  like a bug in the plumbing.
- **The gate's Docker cases must not leak on failure.** `fail` exits the script, so the cases set a global
  `docker_session_id` that the `EXIT` trap removes (container and network); and a case that deletes a live
  session must *wait* for the 404 before removing its scratch root, or the teardown runs against paths that are
  gone and proves nothing.
- **A delete must forget the launch before it removes the container.** `Exits::record` appends to
  `session.log` *before* it classifies and `SessionLog::append` makes the directory, so a live session's exit
  re-creates `<data>/sessions/<id>` after the delete removed it (HS-031's review, P1). Do not "fix" that by
  skipping the log on `Expected` — `tests/exit.rs` asserts the lines are kept for an expected exit.

### Landmines: the mapping and the record seam (HS-032)

- **The transcript's turn is the prompt, and pi's `turn_start`/`turn_end` are per assistant response.** `pump`
  writes the boundary pair; the mapping *declines* pi's. Mapping them makes "turn N" mean "Nth assistant
  response" (the page counts `turn.start`, `turns_before` numbers by it), tells the page a turn settled while
  the row is still `running`, and over-counts the budget notice once a tool round trip exists. The stub emits
  one pair per prompt, and that is the contract.
- **One record must be mapped once.** `pump` maps `record` into a value and matches on it; calling
  `mapping.map(&record)` twice — once to test for `Settled`, once for a frame — mints two message ids for one
  message. The first draft did exactly that.
- **`agent_settled` ends a turn, `agent_end` does not.** pi retries, recovers from an overflow and delivers
  queued messages after `agent_end`; only `agent_settled` means "no work left".
- **A turn's boundary pair is not written when the agent dies mid-turn.** `Received::Closed` answers
  `BackendError::Failed`, the runner writes `unexpected_exit`, and no `turn.end` exists — the turn did not end.
- **The record channel is bounded and the reader *waits* on it.** A `broadcast` ring drops the oldest record,
  which is a transcript with a hole in it and the reason the acceptance's "every delta reaches the writer" was
  not true in the first draft. When the buffer fills, the reader stops draining pi's stdout, which blocks pi on
  its pipe — backpressure, not loss. Do not "fix" a slow pump by loosening that into a lossy ring.
- **`Closed` is the reader's own last word (`Slots::close`), not the channel closing.** The `Agent` value lives
  in the backend's registry for the session's life, so the channel outlives the process; a pump that waited for
  the channel would wait for the session to be deleted.
- **Subscribe before the prompt is written.** pi starts printing as soon as it accepts, and a record printed
  between the write and the subscription is lost from the log too.
- **pi's messages carry no id, so the mapping mints one** (`msg-<first-mint instant>-<counter>`, the stub's
  shape). The process instant is taken on the *first mint*, not at startup — uniqueness does not care, three
  documents once said otherwise. A delta with no `message_start` **opens** the message instead of minting per
  delta.
- **`text_end`/`thinking_end` content is deliberately not emitted.** The deltas already produced the text; the
  only replacing frame is `message.snapshot` and compaction is its only producer (ADR-0032). Emitting the end
  content as a snapshot mid-message would wipe that message's thinking.
- **Two counters, two levels.** A documented record with no frame (`agent_start`, pi's `turn_start`,
  the provider's `toolcall_*` and a tool's partial results, `extension_ui_request` until HS-035, a fixture's
  `response`) is *declined* —
  one `debug` per turn. A type or shape this build has never seen is *unknown* and is a **`warn`**: `debug`
  is invisible at the default `info` filter, and that warn is how a pi release that grew a frame gets noticed.
- **A removal the server performs is never news, and there is one function that removes.** `remove_container`
  supersedes the launch (bumped generation, receiver dropped) *before* it removes anything, and
  `start_container` now calls it — the first draft called the `Docker` CLI helper inline, so the resume path
  was the one place the rule did not reach, and a resume then classified its own `rm -f` as "the container
  exited 137" about one run in two. `supersede_agent`, not `forget_agent`: the generation is derived from the
  registry entry, so forgetting restarts the count at 1 and an old watcher compares equal.
- **The fixtures are captured by hand, and the gate cannot make one.** A session's container has no provider
  key (HS-039) and an `--internal` network (HS-028), so pi refuses `prompt` before a stream exists — that is
  what `case_docker_turn` asserts (pi's own sentence on the row, the container still alive, a second round trip
  after a resume). The streaming evidence is the two committed `.jsonl` fixtures, captured with the operator's
  own credentials on the default bridge from the image the Docker stories run; `docs/build.md` has the
  procedure, and a refresh must not contain the pinned pi version literal (the pin guard scans tracked files).
- **A data root under `/tmp` is invisible to the daemon.** The demo lost a cycle to it: `pi-home` is a mount
  source, so a `/tmp` data root mounts an empty root-owned directory and pi fails with `EACCES` on
  `auth.json` — silently, with no error anywhere but that sentence. Every Docker test, gate case and demo
  builds its data root under `target/`.

### Landmines: tool output, usage and the gate's shell (HS-033, HS-034)

- **A large body in a shell variable kills the gate with no failing assertion.** `expect_header`/`expect_no_cors`
  pipe their argument through `sed '/^\r$/q'`, which quits at the blank line, so a two-megabyte
  `$(cat …)` leaves the writer with SIGPIPE and `set -o pipefail` exits the run **141**, printing only
  `work dir kept:` — no `FAIL` line at all. Both helpers' call sites read the *header block* off the saved file
  now (`sed '/^\r$/q' "$file"`); a new case with a large answer must do the same.
- **Axum decodes `%2F` inside a path segment** (measured, under a mutated `valid_id`): `/tool-output/..%2Fsecret`
  reaches the handler as `../secret`. So an id allow-list really is the defence, and a traversal test can be made
  to discriminate — but only if the canary is where a naive join *lands*. The stored name is `<id>.txt`, so the
  id must omit the extension: `..%2Fsecret` → `<data>/sessions/<id>/secret.txt` (one level up). A canary at
  `<data>/secret.txt`, or an id ending in `.txt`, can never be reached and the test passes with the rule deleted.
  Mutation-check it: replace `valid_id`'s body with `true` and watch the test go red.
- **`wait_for_reason_id` is the *turn* policies' shape** (`idle` + a `status_reason`). An eviction leaves the row
  **`stopped`**, so a case about it wants `wait_for_stopped_id` plus an assertion on the reason.
- **A cap of one means a resume of an evicted session answers `409 cap_reached`** (ADR-0022: the resume refuses
  rather than evicts), so a case that resumes it must delete the session that took the slot first
  (`remove_session`).
- **A fake agent that keeps stdout open and a final record with no trailing LF is a hang, not a bug.** The
  reader splits on LF, so an unterminated last record waits for EOF, and `fake_agent`'s script ends with
  `cat > /dev/null`, which holds the pipe open. Every fixture ends with a newline; a synthetic one must too.
- **`cargo test --test <name>` prints nothing if the run compiled and the test failed earlier** — check the exit
  status, and grep for `test result`, not for a particular line.
- **A doc edit made with a Python string is a landmine of its own**: this session's splice of `docs/build.md`
  put the new case under "the turn timeout's quiet half", so two clauses shared a name. Read the changed lines,
  not the diff's shape (the third time this class of bug has cost a review finding).

### Landmines: images, dialogs and the capture (HS-035)

- **A capture must keep pi's stdin open until `agent_settled`.** `docker run -i < prompt.txt` closes stdin when
  the file ends, and closing stdin is pi's orderly shutdown (`docs/rpc.md`), so the first attempt recorded the
  system prompt and stopped — a `settled: False` and seven records. The working shape is a small driver: write
  the prompt, read stdout line by line, answer a dialog with `{"type":"extension_ui_response","id":…,
  "cancelled":true}`, stop at `agent_settled`. `docs/build.md` carries it.
- **A project's extension only loads with `--approve`**, and the container's own agent directory must be a mount
  at `/tmp/pi-agent` (the image bakes `PI_CODING_AGENT_DIR` there) or the image's own `packages` load and ask UI
  questions of their own. Both were measured here.
- **A body that is not text cannot be compared as text.** The test harness kept only a lossy `String`, so a
  served PNG's `\x89` arrived as U+FFFD and an assertion on it would have passed over a mangled answer.
  `support::Response` now carries `bytes` beside `body`; compare `bytes` for anything binary.
- **The image cap is a *decoded* count and the trailing base64 group counts.** A whole-group computation lets
  `IMAGE_CAP + 2` through on an unpadded body — `the_cap_counts_the_trailing_group…` is the guard.
- **The cap bounds the decode, not the record.** `agent::read_records` reads one line with no length limit, so a
  huge base64 body is already in memory when `decode_base64` refuses it. ADR-0043 records it; do not let a later
  story claim the cap covers the read.
- **A second result replaces the whole set, and the sweep needs digits.** `<tool_id>-<digits>.<known ext>` is the
  only name pattern `tool::sweep` removes; a prefix match would delete another tool's files, and the test keeps a
  neighbour whose id begins with the one being stored for exactly that reason.
- **The `.bin` fallback keeps bytes this build cannot name.** It is deliberate: the frame's `mime` is pi's, and
  the served type is `application/octet-stream`. Do not "fix" it into a refusal.
- **`Cancel::frame` and `ToolEnd::frame` both answer a `Mapped`** so "the one frame this variant answers" has one
  shape; a reviewer suggested matching on a `(kind, payload)` pair instead, and that would be a second spelling.
- **A `turn.error` frame has no message id on purpose** (a steer's shape, ADR-0021): do not mint one to make the
  renderer's `Map<message id, …>` fit, because the log is durable and two shapes of one kind would live in it
  for ever.
- **The review's cheapest true finding was a served-but-unlinked asset.** The wire met "rendered or linked" and
  the page did not, while the ADR claimed it did — read the *demo sentence* of a story against the client, not
  just against the server.

## 7. Decisions taken on the operator's behalf (do not silently undo)

Almost every hard-to-reverse decision is an ADR (`docs/adr/0001` … `0043`, one line each in
`docs/stories/README.md`); read the recent ones before planning, and **amend rather than rewrite** — an ADR
a later story changes gets a "Superseded by ADR-00xx" note (0012/0013, and 0020 was amended by 0026).

The handful that are **not** in an ADR, and that a fresh session is most likely to undo by accident:

- the default branch stays **`master`** — this repo has no `main`, and renaming it would be an invented
  change;
- **never fake a push**: report a failure instead (§2's ssh incantation is how a push actually works);
- **`docs/stories/**` is not edited** — it is the source the Linear issues were imported from; a contract
  that needs changing goes in an ADR;
- the default backend is **`docker`** with the stub opt-in (ADR-0001), so every test and script that boots
  `serve` says which backend it means;
- **S/M stories are implemented in-session; M/L ones go to a worker child with a written brief** (§5);
- the **Linear issue number is not derivable from the story number** — read the title before writing
  `Closes <IDENT>`.

## 8. Resume checklist

```bash
cd /home/adamolsson/projects/holosmith
git status --short                          # must be clean
. ~/.pi/toolchains/env.sh
python3 /tmp/lin.py issues | grep -v ' Done ' | head   # next unfinished (recreate /tmp/lin.py, §2)
python3 /tmp/lin.py issue PER-67            # sanity-check Linear is reachable and scoped
docker version                              # the daemon must answer, or every Docker story is a lie
docker image inspect pi-agent:local         # the image the Docker stories run; its pi == pins/pi.txt
scripts/verify.sh                           # baseline green (~7-9 min, 54 cases, 525 assertions)
```

Then start the cycle at **`PER-68` (HS-036 — per-session network and the startup self-test)** — size **M**.
What is already there and what is genuinely new, so the plan is fast:

- **The first acceptance bullet is done and proven.** `docker::create_internal_network` is
  `docker network create --internal agents_<id>` (reuse on an existing network, because a teardown may have
  failed), the container step joins only that network, and `case_docker_container`/`tests/container.rs` see the
  flags. Do not re-do it; **assert** it where the story's demo wants it.
- **The second bullet is new surface**: the `egress` network (a normal, non-internal network the proxies attach
  to) has to exist. It is *not* per session, so decide where it is created — a `Docker` call in `serve` before
  the listener, or the same place the preflight runs — and record it in an ADR, because "one network for the
  machine" is a shape a later story (`HS-037`'s proxy container, `HS-044`'s reconcile) will depend on.
- **The third bullet is the story's real work**: a *startup self-test* that creates a temporary internal
  network, runs a probe container on it, tries a direct connection to the internet **and to the host**, and
  refuses to start sessions if either succeeds. The refusal belongs with `src/preflight.rs`'s `Complaint`
  (HS-024 already refuses a boot with one sentence per failure, and `role = "hub"` and `--backend stub` are
  exempt) — but it costs a container at startup, so it must be skippable and must not run twice in the gate.
  `tests/preflight.rs`'s `Tools` seam is how a check that needs a machine is tested without the machine.
- **The fourth bullet is HS-044's** ("Orphaned `agents_*` networks are removed at startup (HS-044)") — the
  acceptance names the story that owns it, so implement the self-test and record that the sweep is HS-044's,
  exactly as ADR-0038 recorded `Exits::record` having no caller until HS-031.
- **The fifth bullet is already true and needs a test**: a network step that fails rolls the row to `error` and
  leaves no network (`tests/backend_contract.rs`'s provision step, `case_docker_resume`'s fake socket).
- **The gate cannot assume a daemon.** Every Docker case prints one `SKIP` line and passes without one, and
  `case_docker_preflight` needs no server at all. A self-test that runs at boot will meet
  `scripts/verify.sh`'s own boot, which is `backend = "stub"` — say in the plan how the case exercises the new
  refusal without breaking the run.

Read §1's twenty-two facts first: eight of them are the Docker half (mounts and V1, the container and its
limits, the start lock and `resume`, the expected-stop flag with its classifier, the agent launch and its stdio,
the exit seam with its generations, the agent directory with the data root it forces, and the mapping with its
record seam), one is a tool's output as a file, and the rest are the writer's coalescing, the runner's two turn
policies, the worktree step, the create's project refusals, compaction, truncated replay, the daily bundle and
the rendered page.

**One thing this build does not do, recorded rather than discovered:** a resumed session's pi has **no memory**
of the earlier turns (`--no-session`; the transcript is Holosmith's log). Replaying the log into a new pi is a
product decision with a token cost and needs its own story — ADR-0040 and ADR-0039 both say so, and nothing in
the backlog owns it yet.

**`HS-075` (`PER-107`, expand/contract migration discipline) is independent of all of that** and can be
taken at any point — it is the one story whose only blocker is `HS-004`, and §8's own note below carries the
four decisions and three landmines it needs. It is **not shipped**.

**The remaining true blocker is `HS-066` onwards** (phase 2's deploy/hub stories), which wait on
`HS-047`/`HS-056`/`HS-057` and the phase-1 verification run. Nothing in the queue is now blocked on the
machine itself.

### What HS-075 is about, so the first brief is fast

`PER-107`, expand/contract migration discipline, size **M**, spec item 10, independent of the Docker chain
and **never started**. Acceptance: a release may add but never drop or rename anything the previous
release uses; a test asserts the previous release's schema is still a subset of the current one; `doctor`
reports the schema version and refuses to start when the database is newer than the binary; the rule is
documented where a migration author will read it, with an example of a deferred cleanup.

Four things to settle first:

1. **What "the previous release's schema" is, mechanically.** There is no released artifact and no tag to
   check out, so prefer (a) a checked-in snapshot (`docs/schema/previous.sql`, *not* under `migrations/`)
   compared by a test, over (b) a drop/rename scanner — (a) is a fact, (b) is a lint, and the acceptance
   asks for both. Say in the ADR how the snapshot is refreshed (a release step, not a story step).
2. **Where the check lives**: there is no CI, so a `scripts/check-migrations.sh` run from `verify.sh` as a
   **phase** (it needs no server) plus a `cargo test`.
3. **`doctor` is HS-045's and is blocked.** Implement the *refusal* (the database newer than the binary) and
   the *reporting* (the banner already prints `database … schema n`), record that `doctor` is HS-045's and
   name the sentence it will print. Do not invent a `doctor` command here.
4. **"Where a migration author will read it"** is `docs/build.md`'s migrations section plus the ADR, and the
   deferred-cleanup example should be a real pair of migration files or a quoted diff.

Two traps: `scripts/check-static.sh` byte-greps the artifact for the schema SQL, and `SCHEMA_VERSION` is a
literal `check_embed` cross-checks against the embedded files — so a snapshot under `migrations/` would be
embedded as a migration and break the build.

## 9. Reporting

The durable record is **Linear** (plan, demo output and ship note per issue), **`docs/review/PER-<n>.md`**
(the review verbatim and its `## Resolution` table) and **`git log`**. Keep a short ledger in the
conversation while a session runs (issue → plan → implement → review → fix → ship) so a restart can see it
from those three; do not carry old sessions' ledgers in this file — that was 200 lines of history nobody
re-read.

If you stop before the backlog is empty, say so explicitly, say which issue is next, and leave the tree
clean and the gates green — **a half-finished story is worse than an honest stop**.

This session (`b2fecf5` → `60eaac3`) shipped one story:

| Issue | Story | Commits | Outcome |
| --- | --- | --- | --- |
| PER-67 | HS-035 images and extension dialogs (V8) | `70f337e`, `c0b72af`, `f9d64aa`, `6fa4a89`, `60eaac3` | Done — an image in a tool result is a file beside the tool's text (`<tool_id>-<block index>.<ext>`, the base64 decoded by the store, the cap counted on decoded bytes including the trailing group and refused from the encoded length), named by `tool.end`'s `images` additively, served by `GET …/tool-image/{tool_id}/{index}` with the type the *file* has, and drawn by the page as one link per picture; an extension's dialog is cancelled with the response written **before** the frame, because pi blocks on it, and the fire-and-forget methods stay declined; the contract gained `turn.error` (`{reason, method}`, no message id). Two real fixtures captured (an image in a tool result, a `select` dialog whose `notify` records the cancel). ADR-0043. Review: **OK with notes** on both axes, one P1 (the picture was served and never linked while the ADR claimed it was) and one P2 (a second result replaced only the indices it carried) — both fixed in code with the test that makes the wrong design fail, six P3s fixed and one dismissed (`docs/review/PER-67.md`) |

Ledger for the next session: HS-036 (`PER-68`) is the next issue and the first of the network half; §8's own
note says which of its five bullets is already true, which is HS-044's, and where the startup self-test has to
live without breaking the gate's stub boot. Then `HS-037` on, in `HS-###` order.

