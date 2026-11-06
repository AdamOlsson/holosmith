# Notes for a cold start

`RESUME.md` is the state of the world and it names the **one** issue a session picks up. This file is the
rest of what a cold start must know that is *not* state and *not* already in an ADR:

- the **shape of the build** at the last handoff (as of `PER-72`, the credential path) — the facts whose
  home is prose rather than a decision;
- the transcript's **event contract** and the rule about who owns a state change;
- the **landmines**: measurements and mistakes that cost real time, grouped by the part of the build they
  belong to.

It is a reference, not a briefing. `RESUME.md` §6 says which parts bear on the issue at hand; the rest is
for when a test fails in a way you have not seen before.

Cross-references: §1–§5 are `RESUME.md`'s sections — the issue, the environment, the gates, the repository
and the cycle. `item N` always means an item of `HolosmithSpec.md`. `fact N` means a fact below.

---

## The build's shape (thirty facts, as of PER-72)

Not decisions — the ADRs are decisions, one line each in `docs/stories/README.md`. These are what the code
now does, and each names the ADR or the review that owns it.

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
    `docker run` (the landmines' mount rule: `-v` creates a missing source as an empty root-owned directory).
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
    sentence on the row — a session's network is `--internal`, its pi-home has a dummy key, and the gate's
    machine stores no provider key, so `models.json` has an empty provider object and pi rejects the `prompt`
    command before a stream exists. That is a different observable from a hang, and the streaming evidence is
    the committed fixtures (HS-040 owns the real-key turn).

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

24. **A Docker boot measures the machine's isolation, and only a measured leak refuses it** (HS-036, ADR-0044).
    `src/selftest.rs` creates `holosmith-selftest` (`--internal`, reused then always removed), runs **one probe
    container from `agent_image`** on it with `templates/selftest-probe.js` as `node -e`, and asks it to reach
    `1.1.1.1:443` and the default bridge's gateway on `2375`. **A `refused` connection counts as reached** — a
    reset proves the host answered — while `ENETUNREACH`/a timeout are the pass; measured both ways, so
    `tests/selftest.rs` runs the probe on the bridge too as its control. Three outcomes and one refusal:
    `Passed` (a `debug` line), `Unknown` (no image, a daemon that cannot create networks, or the probe exceeding
    `PROBE_TIMEOUT` = 60 s, which exists because `docker run` on the startup path must not hang a boot) → one
    `warn` and the boot continues, and `Failed` → a `preflight::Complaint` with `Check::Network`, printed and
    refused exactly like the other three. The **`egress`** network (`docker::EGRESS_NETWORK`, not internal, for
    the proxies of HS-037/039) is created at the same point, with a `warn` rather than a refusal. `role = "hub"`
    and `--backend stub` do neither. **Measured limit, recorded in ADR-0044 and not to be "fixed" by adding a
    target**: an internal network's *own* gateway answers (`refused`) from inside it, so item 2's "no gateway to
    the host" is really "no route out" — anything host-side bound to `0.0.0.0` is reachable at that bridge
    address, and item 4's guards, not the network, are its defence.

25. **The only route out is a proxy this build starts, and it is a mode of this binary** (HS-037, ADR-0045).
    `src/egress.rs` is the proxy: pure rules (`Request::parse`, `Rules::target`, `address_problem`) plus a
    thin tokio layer (`serve`/`handle`/`Record::tunnel`), and `holosmith proxy egress --listen --allow …` is
    how it runs — the spec's own "internal subcommand" spelling, so one artifact is both server and proxy. Its
    container is `hs_egress` on `egress`, `--detach --restart unless-stopped --cap-drop ALL
    --security-opt no-new-privileges`, running **this process's own binary** mounted read-only at its own host
    path (`EgressProxy::of(&Config)`, `PROXY_START_TIMEOUT` = 90 s because it starts before the listener
    binds). **`CONNECT` to 443 only** (405 for another method, 403 for another port), an **exact** allow-list
    (a suffix match is the wildcard `config validate` refuses), and **no address inside the machine** —
    loopback, 169.254/16, RFC 1918, 100.64/10, IPv6's own spellings and the IPv4 inside 6to4/NAT64/
    IPv4-compatible ones, with **every** answer a name has checked. Attribution is `Proxy-Authorization`'s
    username (validated by `tool::valid_id`), which is why a session's container gets
    `HTTPS_PROXY=http://<id>@egress-proxy:3128` — `egress-proxy` is a **network alias**, the container's name
    is `hs_egress`. The join is the **network step's** (`provision_network`/`resume`/`remove_network`, under
    the per-session start lock) and runs only when the row's `network` flag is set; every failure is a `warn`,
    never a refusal. Both the start and the self-test's probe are **bounded** (`Docker::bounded`), and the
    request log is the *container's* (`docker logs hs_egress`), not the server's.

26. **"Off" is measured at boot as a *pair*, and the pair is what makes it evidence** (HS-038, ADR-0046).
    `selftest::PROXY_TARGET = "egress-proxy:3128"` — the **alias**, never `hs_egress`, because the container name
    resolves on the `egress` network only and a probe asking for it would pass on every machine — joins the probe's
    target list, and the probe runs **twice on the same network**: bare (nothing may answer, the alias included)
    and then with the machine's proxy joined (the alias alone must answer). A silence on a network nothing is
    attached to is only evidence if it is not the silence of an alias that was never created; `decide`/`leaked`
    give a proxy that answers its **own** sentence instead of the bridge advice, and a direct leak still outranks
    it. The second run is a `debug` line when it works and a `warn` when it does not, **never a verdict** — a proxy
    that will not join fails a download, not a session (ADR-0045). **HS-039 added the key proxy's inverse pair**
    (`KEY_PROXY_TARGET = "key-proxy:8080"`, joined to the same network and required to answer) rather than a
    target nothing may reach. The
    session-shaped claim is the gate's: `case_internet_toggle` creates two real sessions differing only in the
    body's `network`, and asserts per network the internet unreachable directly, the alias resolving in exactly
    one, every env var identical once each id is normalised, and — after the proxy is **detached** and the
    container killed and resumed — the network's endpoint count **1** off / **2** on. Detaching first is
    load-bearing: `resume` re-creates the network idempotently, so an `on` session would report 2 with the re-join
    deleted. The copy under the create form was **cut** (a new element is a UI change in a phase that forbids one);
    the phrase an operator reads is `transcript.ts`'s `networkText`, tested.
27. **Expand/contract has two halves, and only one of them is about one file** (HS-075, ADR-0047).
    `schema::contract_violations` (HS-004) already refuses `DROP TABLE`, `DROP COLUMN` and `RENAME` in a migration
    whose **slug** is not `contract`/`contract_*`, matched on words, and `cargo test` runs it over the embedded
    set. The other half is a fact about *two* releases: `docs/schema/previous.sql` is a real dump of the previous
    release's schema, kept **out of `migrations/`** (that directory is embedded and `check-static.sh` byte-greps the
    artifact), and `tests/schema_previous.rs` compares it with this build's as a **subset** through SQLite
    (`sqlite_master`, `pragma_table_info`) — a drop or a rename fails, an addition does not. **A first draft
    compared table sets for equality and forbade an added table**; the review's P1, and the reason the comparison
    is one direction with two truncation guards that are not the rule's opposite. `scripts/schema-snapshot.sh`
    refreshes the file **at release time** (read-only connection, `uname -m` triple, `.tmp` moved into place), and
    a contract migration changes it in the same commit — the machine can enforce *deliberate*, never *one release
    later*. `schema::verdict` is the other half's vocabulary: `Same`/`Older`/`Newer` with one sentence each,
    `apply_pending` returns early on `Same` and warns-and-continues on `Newer` (**item 10 needs that**: a rollback
    must not need a restore) and `doctor` is where `Newer` refuses. The gate has a `migrations` **phase** and
    `case_schema_newer`, which boots the artifact over a schema-99 database.

28. **The key proxy keeps the real key out of every session, and it is a second mode of this binary** (HS-039,
    ADR-0048). `<data>/secrets/keys.toml` (0600) holds `[providers]` (real keys) and `[sessions]` (each session's
    dummy); `holosmith key set|list|remove` edits it, `set` reading stdin only. `hs_key` mounts that file
    read-only at its own path on the `egress` network, is joined to **every** session network under the alias
    `key-proxy`, and `holosmith proxy key` validates the dummy, swaps in the real key in the provider's own
    header, and forwards over TLS with `openssl s_client` (the artifact is static). The provider is a fixed
    table (`key::PROVIDERS`, host + base path + auth header), never a request-controlled host. `models.json` in
    the pi-home is rewritten on every start with the proxy URL and the dummy; `settings.json` keeps the
    seed-only-if-absent rule. The delete flow's `revoke_key` removes the session's row. A corrupt store fails
    the container step with the file named; a missing proxy is a `warn`.
29. **The WIP snapshot is a temporary index and a ref, and it is taken before the work can be lost** (HS-042,
    ADR-0049). `src/snapshot.rs` implements spec item 9: `GIT_INDEX_FILE` for `read-tree HEAD`, `add -A` and
    `write-tree`, then `commit-tree` with `holosmith <holosmith@localhost>` and `update-ref` to
    `refs/holosmith/wip/<id>`; a tree equal to `HEAD` removes the ref. It runs on every `agent_settled`
    (`Turn::finished`), before every stop (`Stopper::stop`) and before delete (the existing 409 path). The host
    runs it; `holosmith container snapshot` is the `/work`/`HS_SESSION_ID` equivalent. A failure is logged on a
    settle/stop and is a refusal at delete.
30. **The credential path is verified, and the run that verified it found two defects** (HS-040, ADR-0050). V3,
    V4 and V5 pass against a real `opencode-go` key, and the spec's vertical-slice table now has a **Result**
    column (V3/V4/V5 filled; the rest say which story fills them, V11 deferred to phase 3). Two things changed in
    the code, both P1-class and both invisible to the gate as it was: **`keyproxy::upstream` returned a head with no
    body** (so every model call was `502 the provider closed the connection before answering`), and **`hs_key`
    mounted `keys.toml` as a file** — a bind mount pins the inode, and the store is written by temp-file-and-rename,
    so the proxy kept reading the store from before the first create and answered `session=- … status=bad-key` for
    dummies it had minted itself. `KeyProxy::mount` is `keys`'s parent now, and `--keys` still names the file. The
    provider table gained an **`opencode-go`** row (`opencode.ai`, `/inference/go/anthropic`, `x-api-key`): a row is
    one (host, path, auth) triple, therefore **one API shape** — the openai-completions shape would be a second row.
    Measured precedence is `auth.json` > `models.json` `apiKey` > the provider env var, which is why the seeded
    pi-home has no `auth.json` and the container environment no provider key; that is V5's documented fallback, and
    it is a rule about what the build keeps out rather than a repair. The gate's half is `case_key_proxy_request`:
    a stand-in `openssl` on `PATH` captures the bytes the proxy really sends (one `Content-Length`, the body after
    the blank line, the stored key swapped in) — no key, no internet, no daemon. `docs/build.md` carries the
    by-hand procedure; the checked-in demo is `/tmp`-only, and its recorded output is in Linear.

---

## The transcript's contract and the state rule (HS-014, HS-015)

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

---

## Landmines (each of these cost real time)

Every entry here is a measurement or a review finding, not a preference. When one stops being true, delete
it rather than adding a caveat beside it.


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
  key means updating the golden file and `KNOWN_KEYS.len()`** (thirty-two today, asserted in
  `config/schema.rs`).
- **`docs/build.md` is a story deliverable, not orchestrator-only.** If a story changes prerequisites or
  gates, the doc is in scope, and its prose must be reflowed (~100 columns), not just appended to.
- **Every story adds at least one `case_*` to `scripts/verify.sh`.** The rule is in bold in
  `docs/build.md`, and the reviewer enforces it. The gate prints its own case and assertion counts —
  quote the run, never a number remembered from a document — and a case that makes its own session must
  run before the shared one exists or delete it at the end (the gate's cap is one live session).
- **Adding an ADR means updating `docs/stories/README.md`**: the "Decisions taken while slicing" count
  (now **fifty**) and the bullet list. A reviewer will check the links resolve — three did not,
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
- **Nothing a session starts outlives it — and the reason is the next agent, not tidiness.** A `worker` or
  `reviewer` child still running keeps editing the tree the next session has to trust, which is why the
  cycle's last step stops every run (`subagent({action:"list"})`, then `stop`); a stranded demo `serve`
  holds its port and its data root, so the next `serve` fails on the bind; and a `s_<id>` container or
  `agents_<id>` network left behind makes a later `case_*` answer about the wrong machine. Score the pids
  when you start them (a `pgrep` pattern matches unrelated processes, above), and check with `docker ps`
  rather than assuming.
- **An async child cannot be killed by stopping to talk to it.** `stop` ends the run and is not resumable,
  so at the end of a session it is always safe; what is *not* safe is assuming a child that stopped
  reporting has stopped working. `subagent({action:"list"})` is the measurement, and an empty list is the
  only evidence that nothing of yours is still running.

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

### Landmines: the network self-test (HS-036)

- **A gateway field is not a measurement.** An `--internal` network reports an IPAM gateway and that address
  *answers* from inside it (`refused`, measured). So do not "improve" the check into an assertion on a field, and
  do not add the internal gateway as a probe target — it fails on every Docker installation, which is a lockout
  rather than a test. The ADR records the limit and who owns closing it.
- **`connected` and `refused` are both "reached".** A probe that counted only `connected` would pass a machine
  whose containers reach the host, which is the leak the story is about.
- **A startup `docker run` needs a deadline.** The probe bounds itself per target, but a daemon that accepts a run
  and then wedges hangs the boot before the listener binds — `Docker::attempt` polls `try_wait` under
  `PROBE_TIMEOUT` and a bound that expires is `Unknown`, never a verdict.
- **"Could not measure" is not "measured a leak".** Only `Failed` refuses the boot; `Unknown` is one `warn`. That
  is also what keeps every gate case that boots Docker against a `_ping`-only socket working — check that
  property before changing the level.
- **A `warn!` at startup is visible and an `info!` breaks a test.** `tests/serve_cli.rs` pins a *stub* boot's
  banner at exactly eight lines; a Docker boot's banner is not pinned, so the self-test's lines are `debug`/`warn`
  and the case that wants to see the pass boots with `HOLOSMITH_LOG=debug`.
- **The probe is a node script and the image must have node.** A missing node is exit 127, which is `Unknown` (a
  `warn` at every boot), not a test failure — so `docs/build.md`'s image prerequisites name it.
- **The `egress` network is the machine's, and its failure is a `warn`.** Refusing the boot over it would take the
  control UI down, which is the one screen an operator needs to see what happened.
- **Orphaned `agents_*` networks accumulate.** Two from earlier gate runs were on this machine when HS-036
  finished; HS-044 owns the sweep.
- **A started server's data root cannot be removed while it runs.** `case_network_selftest` leaves its scratch
  root because the phase's final `stop_serve` is what ends that server; it cleans at the *start* of the next run,
  like `case_docker_turn`.

### Landmines: the egress proxy (HS-037)

- **A `200` to `CONNECT` carries no body at all.** It is the status line and a blank line, and everything after it
  is the client's TLS handshake — a `Content-Length`, a `Content-Type` or a `Proxy-Agent` there is read as the
  first TLS record and `curl` says `error:0A00010B:SSL routines::wrong version number`. The story's own demo
  found it; no test that only probed refusals would have, which is why `Record::tunnel` is now a `pub(crate)`
  function a test drives against a loopback echo listener (below the policy, which is what makes it possible).
- **A `copy` that fails reports no count.** `tokio::io::copy_bidirectional` answers `(0, 0)` when either direction
  errors, so the request an operator reads the line for (one that carried a megabyte and then died) logs
  `bytes_up=0`. The tunnel is two counted loops over a split socket (`pump`, `pump_both`) that bump an
  `AtomicU64` as bytes move.
- **`tracing`'s `field = value` uses `Debug` for a `&str`, and prints it quoted.** `status = self.status` rendered
  `status="not-listed"`, which a `grep status=not-listed` misses; `status = %self.status` is the fix. Any field a
  gate greps needs `%`.
- **The proxy's log is the container's.** `docker logs hs_egress` (or the process's stderr for a host-side run) —
  **not** `serve`'s stderr, which is where the boot's `debug`/`warn` lines about starting it live. A demo that
  greps `serve.err` for "answered a request" finds nothing and looks like a broken proxy.
- **`egress-proxy` is a network alias, not a container name.** `docker network connect --alias egress-proxy`, and
  Docker's embedded DNS answers an alias only on networks the asker shares with the container. `docker inspect
  hs_egress --format '{{(index .NetworkSettings.Networks "agents_x").Aliases}}'` is where it is visible; a
  resolution test from inside a session container (`node -e require('dns').lookup('egress-proxy')`) is the
  measurement (done in the demo: `172.21.0.2`).
- **`docker network rm` refuses a network with a live endpoint.** The egress proxy is one for as long as it is
  joined, so a teardown that forgot the detach would leave the network behind with a `warn` — and ADR-0023 says a
  delete leaves no `deleting` row. `remove_network` leaves first, and `tests/container.rs` asserts both halves by
  driving `SessionBackend::undo(Step::Network)`.
- **`--restart` and `--rm` conflict.** The proxy is detached with a restart policy and no `--rm`, so a stopped
  proxy keeps its name; the boot's `rm -f` (bounded) is what clears it.
- **The proxy container runs *this* binary, mounted read-only at its own host path.** So the daemon must be able to
  see that path — the same fact ADR-0035/0039 record for the projects and data roots. A `serve` whose binary is on
  a tmpfs the daemon does not share starts no proxy: one `warn`, and `serve` checks `is_file` first, because `-v`
  of an invisible path creates an empty root-owned *directory*.
- **Everything on the startup path is bounded.** `Docker::run_detached` and `Docker::remove_container_bounded` take
  a deadline (`container::PROXY_START_TIMEOUT`, 90 s) for the reason `Docker::attempt` does: a `docker run` can
  have to pull, and the control listener is the screen an operator needs when it cannot.
- **A daemon-gated test must not take the production name.** `hs_egress` is the machine's live proxy; the test uses
  `hs_egress_test` and skips when `docker.running(PROXY_NAME)`, or `cargo test` on a machine with a running server
  would replace its proxy.
- **The allow-list is exact, so a registry needs every name.** `crates.io` does not permit `static.crates.io`. That
  is deliberate (a suffix match is the wildcard `config validate` refuses); a story that "fixes" it into a suffix
  match re-opens the decision ADR-0045 records.
- **Two things the proxy cannot do, and what owns them.** It cannot inspect a tunnel's content (item 2's accepted
  risk), and it cannot stop a container from reaching the host's own bridge-bound services at its network's
  gateway (ADR-0044's measurement) — item 4's guards are that path's defence and **HS-046** owns the decision.

### Landmines: the checkbox's pair, and expand/contract (HS-038, HS-075)

- **A gate case that SKIPs on the bug it exists to catch is the P1 of this project.** HS-038's `on` half skipped when
  the proxy's alias did not answer — which is exactly what a deleted `join_network` produces. "The alias does not
  answer" has two causes and `EAI_AGAIN` cannot tell them apart; one `docker inspect hs_egress` can, and a SKIP now
  has to be backed by a line the boot actually printed. Before you write `SKIP` inside a case's assertion, ask what
  a missing feature would print there.
- **A measurement that cannot distinguish "absent" from "never created" is not a measurement.** HS-036's probe
  reported silence on a network nothing was attached to, and so would a build whose alias was never added — so
  HS-038 made it a **pair**: bare, then with the proxy joined, on the same network. The same review found the
  endpoint count inherited its "2" from the create, which is why the case detaches the proxy before the kill.
  **Mutate the wrong thing and watch the test stay green** — that is how both were found.
- **A non-vacuity guard must not be the rule's opposite.** HS-075's first subset test added a guard that compared
  table *sets* for equality, which forbade the one thing expand/contract permits (adding a table) and pointed its
  repair at refreshing the snapshot — the move that voids the check for a release. The reviewer's P1. A guard is
  allowed to demand that the fixture is non-empty; it is not allowed to contradict the rule.
- **`support::TempDir` is keyed by label *and* pid**, so two tests in one binary that share a label delete each
  other's directory mid-run. SQLite's answer is `disk I/O error`, which reads like a filesystem bug. One label per
  test.
- **A SQL rule has one implementation.** `schema::contract_violations` already existed and was already run by
  `cargo test`, so HS-075 built **no second scanner** and no shell script — `scripts/verify.sh`'s `migrations`
  phase runs the Rust test. A `sed`/`grep` twin of a SQL rule is the second spelling this repo refuses.
- **Anything embedded is off limits for fixtures.** `migrations/` is `include_dir!`-ed and `check-static.sh`
  byte-greps the artifact for its SQL, so a schema snapshot lives in `docs/schema/` and is loaded with
  `include_str!` by a test only.
- **`git describe` is baked into the artifact, so a gate run on a dirty tree produces a `-dirty` banner** — and
  `tests/pin_single_source.rs` scans *saved reviews too*: a review that spells the pinned version literal makes
  `cargo test` red. Redact it in the review body and say so in the `## Resolution` (HS-075 had to).
- **A `local a=… b="$a"` is unbound under `set -u`** — hit again in HS-038 (`local root=… projects="$root/…"`), and
  the fix is two `local`s. Where a shell `case` reads verdicts an embedded node script produces, the words have to
  be shell variables; `$CONNECTING` from a heredoc is an unbound variable, and an *accepted-anything* pattern
  (`"direct "*)`) is how a case passes over a probe that changed its words.

### Landmines: the credential path (HS-040)

- **A request that declares a length must carry the bytes in the same value.** `keyproxy::upstream` wrote
  `Content-Length: body.len()` and returned a head; every model call was a request the provider waited out and
  dropped (`502 the provider closed the connection before answering`, ~16 s later). The tests passed because they
  asserted the header. Assert the **tail**, and that the length appears exactly once.
- **A file bind mount cannot see a rename, and this build writes its important files by rename.** `keys.toml`
  (temp + rename, ADR-0048), the bundles (`.tmp` + rename, ADR-0034) and the schema snapshot are all written that
  way; a container that must read one mounts the **directory**. The measurement is one log line:
  `session=- … status=bad-key` for a dummy the proxy minted itself means it is reading a stale inode.
- **A gate that needs a key is not a gate, but a gate that proves nothing is worse.** `case_key_proxy_request`
  puts a stand-in `openssl` on `PATH` (the TLS client is a process, `Command::new("openssl")`) and captures the
  bytes the proxy really sends — the seam `preflight::Tools` established, used on the *shell's* side where setting
  `PATH` is safe. Give the shim one bounded `dd` read, not a read-until-EOF: the proxy keeps its child's stdin open
  on purpose.
- **Credential precedence is a measurement, not a reading.** `auth.json` > `models.json` `apiKey` > the provider
  environment variable (measured against the proxy's own log). The docs' order for the last two is right and the
  *first* is the one that matters: a seeded pi-home with an `auth.json` would bypass the proxy with a real key —
  which is why the seed writes only `settings.json` and `models.json`.
- **`opencode-go` is a provider with two API shapes and needs an `x-opencode-session` header pi mints itself.**
  A row is one (host, path, auth) triple so it serves one shape; `claude-haiku-5-5` under
  `/inference/go/anthropic` is the anthropic-messages one, and it only emits thinking deltas at `--thinking max`
  (at `xhigh` it answered without thinking, and an easy prompt produces none at any level — ask it something that
  needs reasoning or the check measures the prompt rather than the wire).

### Landmines: the preview listener (HS-041)

- **The host *can* reach a container on an `--internal` network — measured, once, both ways.** `docker inspect`
  gave `172.18.0.2` on `hs_v7probe`, the host fetched `http://172.18.0.2:8099/` and got `200`, and the same
  container's own connection to its gateway `172.18.0.1:8099` was `ECONNREFUSED`. ADR-0044's "a refused connection
  counts as reached" is why the second line matters: an internal network's gateway answers *from inside it*, so
  "cannot reach out" was never evidence about "reachable from in". V7 is the second fact only, and it passes.
- **The preview listener is not an `axum` app, and adding a route to it is not a thing you can do.** It is a raw
  TCP tunnel (`src/preview.rs`) like `egress` and `keyproxy`, with no route table at all; the only paths that exist
  are the ones `Route::strip` accepts. Its refusals are **plain text**, not ADR-0008's envelope — the envelope is
  the control listener's contract, and a preview is a page an operator opens on a phone.
- **A body can arrive in the same read as the head.** `tunnel::read_head` returns everything it read, so anything
  relayed from that buffer must be split at the blank line (`preview::split_head`) or a `POST`'s body is parsed as
  header lines and mangled. `a_body_that_arrived_with_the_head_still_reaches_the_dev_server` is the test.
- **`docker inspect` with a network index is brace soup; use a placeholder.** The format for a container's address
  on *one* network is `{{with index .NetworkSettings.Networks "agents_<id>"}}{{.IPAddress}}{{end}}`, and writing
  that through `format!` needs four escaped braces per call. `preview::Containers` keeps the literal and replaces
  a `NETWORK` token instead.
- **A preview port of `0` is not a collision with a control port of `0`.** `preview_listen_problem` compares the
  two `SocketAddr`s, so it now exempts either port being `0` — the kernel picks different ones. Without that,
  every test and every case would need a fixed preview port and would race whatever else on the machine held it.
  `scripts/verify.sh` writes `preview_listen = "127.0.0.1:0"` and reads the real port from the banner.
- **`case_preview` makes its own container, because the shared boot's backend is the stub.** The container is
  `s_<id>` on `--internal` `agents_<id>`, named the way the convention names a session's container, and the case
  asks the host *directly* first so a failure says which half is wrong. Its WebSocket half is a hand-written
  handshake in `python3` and a hand-written echo in the container's `node -e`: the image has no `ws` package and
  this build has no WebSocket crate, and that is exactly why a byte tunnel is the right shape.
- **A demo's trap must remove the container, not only the server.** The first run of this story's demo exited
  through a failed `curl` (a container's address is assigned before node is listening), so the trap killed `serve`
  and left `s_<id>` and `agents_<id>` behind — and the next gate run failed two pi-launch tests with "pi did not
  answer within 30 seconds" for a reason that belonged to the *previous* session. `trap` on the container and the
  network, and check `docker ps` before believing a red launch test.
- **The container is told the preview port (`HS_PREVIEW_PORT`), and that is the half people forget.** The config
  pins one number (`preview_upstream_port`), the listener dials it, and `container::run_args` passes it in beside
  `HS_SESSION_ID` — a pinned port the container was never told is a preview nobody can open. An agent that picks a
  port of its own builds something no one can look at.
- **`preview_origin` is the published origin, never the bound port.** `--port 0` re-derives `control_origin` because
  the flag asked the kernel for a port; nothing does that for the preview, because the origin is the name a phone
  browses (`tailscale serve --https=8444` maps to it). A case that binds `127.0.0.1:0` and sends `Origin:
  http://localhost:8081` is therefore correct behaviour, not a mismatch.

### Landmines: what a delete keeps (HS-043)

- **The keep-rule lives where the branch is deleted.** `Worktree::retained(base, targets)` answers
  `Retained::{Known { branch, wip }, Unknown}` and `remove(&retained)` is told what **not** to delete, so
  `tests/worktree.rs` proves the rule on a `TempDir` repository with no daemon, no backend and no request
  (ADR-0029's reason, ADR-0052's shape). A `Deleter` that ran git itself would need the repository path
  (the backend's), the targets (the config's) and a `spawn_blocking` of its own.
- **A test that only asserts "nothing was kept" can pass for the wrong reason.** Two rules agree on the
  common case: a branch with no commits beyond its base *and* an ancestor of its target answers `None`
  either way, so deleting the `ahead == 0` arm left the test green (found by mutation). The case that
  isolates it is a repository whose branch is *not an ancestor of any target* — a repo on `trunk` with
  `[merge] targets = [main, master]`, where the "no target can answer" rule would keep the branch.
- **Three git commands in this module exit non-zero as an answer**, and each must be read rather than
  treated as a failure: `rev-parse --verify --quiet` (1 = no such ref), `merge-base --is-ancestor`
  (1 = not an ancestor) and `diff --quiet <ref> <branch>` (1 = the trees differ). `support::git` panics on
  a non-zero exit, so a test that wants "not there" asserts a helper's `false`, never a `git` call.
- **`[merge] targets` names both `main` and `master` because a machine has one of them.** A target the
  repository does not have is **not** a merge, and a repository with none of them cannot answer at all —
  which keeps the branch. Treating a missing target as "merged" deletes every unmerged branch on half the
  machines, so the absent-name case has its own test.
- **The keep-rule is computed after the snapshot and handed to the spawned task.** The snapshot is what
  writes or removes `refs/holosmith/wip/<id>`, so computing the answer before it makes `wip` null every
  time (proved by mutation: moving the one line to before the snapshot fails
  `the_delete_answer_names_the_work_it_keeps_and_the_command_that_recovers_it`), and a task that recomputed
  it could disagree with what the operator was just told.
- **The 202's `kept` is an additive key on a flattened row**, in `http/sessions.rs`'s `DeleteAnswer`.
  `Session`'s own serialisation is pinned to fifteen columns in column order by three tests, so the key must
  not go on `Session` — and `kept: null` (the repository could not answer) is a *different* answer from
  three nulls (nothing survived), which the page renders differently.
- **The shipped page's script asset is a minified bundle.** It keeps string literals and mangles
  identifiers, so a gate case may grep it for copy (`Delete session`, `recover it with`) and never for a
  function or variable name. Vite may also split assets, so the grep is over `web/dist/assets/*.js` rather
  than one file.
- **`[hidden]` is the last rule in `style.css` on purpose, and it has the same specificity as a class
  selector.** A new block that sets `display: flex` must be written **above** it (as `.confirm` is), or the
  confirm step stays on screen when `hidden` is set.
- **A test that stands in for a named container must *be* that name, or its assertions cannot hold.**
  `tests/container.rs`'s egress-proxy test skipped whenever a proxy was running (so it never replaced the
  machine's own), and its stand-in was called `hs_egress_test` — while the detach `remove_network` performs
  is `docker network disconnect … hs_egress`, by name. So on a machine with **no** proxy running the network
  could not come off and the assertion failed, and on one **with** a proxy it skipped: the real path never
  ran. Found at HS-043's baseline. The stand-in is `PROXY_NAME` now (safe only because of the SKIP), and the
  lesson generalises: an assertion about a teardown that names something is an assertion about that name.
- **The machine's `egress` network is part of the baseline, and it can be missing.** `serve` creates it at a
  Docker boot (ADR-0044) and a cleanup that removed it makes `tests/container.rs`'s proxy test fail on setup
  ("network egress not found") before any assertion. `docker network create egress` is the same call the
  boot makes; a gate run's own Docker boots recreate it, but `cargo test` runs *before* them.
- **`StreamTarget` carries one `showState` now, not `badge` + `updateComposer`.** The delete affordance was
  the fourth reader of "a state changed", and the split is what HS-020's blocker was about; a fifth copy
  would be the same bug with a new name.

### Landmines: the startup reconcile (HS-044)

- **A sweep that runs as a background task beside a create takes the worktree the create just made.** The pass
  reads the rows once at the top; a create accepted in the first milliseconds of a boot inserts its row and
  runs its worktree step while the sweep still holds that snapshot, so the sweep sees "a directory no row owns"
  and removes it — and the create's container step then fails with `the worktree cannot be mounted: … is not
  there` (this story's own demo, first run). The pass is therefore **awaited before the listener answers**
  (ADR-0053), not spawned like the sweeper; its restarts are still `Provisioner::spawn` tasks. A future
  background caller (a `doctor`, a periodic sweep) has to re-read the rows *and* the names under one lock-free
  ordering of its own, which is why the row and name questions are separate pure functions.
- **The pass must also run before the two startup policies, or the sweeper takes a row it is about to bring
  back.** The idle-stop sweeper's first pass is immediate and its candidates are `idle` rows (ADR-0025), so a
  boot whose live row is past `idle_stop_min` would have it `stopped` *before* the crash transition — which is
  then refused as illegal, and the restart never happens. `serve` therefore awaits the reconcile right after the
  state exists and only then spawns the sweeper and the compactor.
- **A recovery parks every live row in `starting`, so compaction's eligible set had to learn `starting`.**
  `foldable` was `idle|stopped|error`, and the compactor's first pass is immediate at startup (ADR-0032), so a
  restarted machine with a day-old log folded nothing until the 60-second tick — `case_compaction` failed with
  "no message.snapshot row appeared". A `starting` row has no turn running (a prompt needs `idle`) and no removal
  in flight, and the fold takes the same mutex, so the state is folded now; the rule is ADR-0032's.
- **The crash's reason has to be written twice or the row loses it.** The transition into `starting` records the
  crash, but the re-provision's own `starting → idle` clears `status_reason` (`set_status` writes the reason it
  was given, and the provisioner gave `None`), so the row would come back plain `idle` and the page would show
  nothing. `Provisioner::with_settle_reason` is the fix, and `Transitioner::apply` is still the only writer; the
  failure ending deliberately keeps the failure's sentence instead.
- **A row is ahead of its event, again.** `tests/reconcile.rs` waits for the *payload* to be committed before
  reading the log (`wait_for_payload`), because "the row says idle" is one writer tick earlier than the event.
  The same applies to `sessions.last_seq`: read the counter only after the settle's event landed.
- **The counter had no reader.** `sessions::repository::bump_last_seq` is `pub(crate)` (exactly one writer), so
  a test could not read `last_seq` at all; `last_seq(id)` was added for this story's claim and for HS-046. The
  gate reads it with `python3`'s `sqlite3`, like `case_exit_classification` reads `expected_stop`. It is
  the sessions repository's because it is a `sessions` statement, and `MAX(seq)` is *not* the same number once
  compaction has removed rows (ADR-0032).
- **`sessions::is_id` is the one answer to "what is an id".** The sweep decides "the build's name or not" with it
  (16 lowercase hex), not with a second copy: Docker's `name=` filter is a substring match, so `name=s_` also
  answers a container called `hs_s_things`, and the prefix-plus-shape check in Rust is the rule.
- **`git worktree remove --force` of an *unregistered* path is not a removal** (git exits 128), so the sweep
  follows every removal with `remove_dir_all` for a directory git never registered — the shape a killed start
  leaves — and `Retained::Unknown` is how it keeps the branch when there is no row to ask (ADR-0052).
- **`worktree list --porcelain` prints raw, resolved paths** (HS-025), so the sweep compares canonicalised
  *parents* against `<projects>/.worktrees` — the same measurement `Worktree::registered` already lives with.
- **A stub boot must not sweep anything, but it must still reconcile the rows.** A hub has no daemon and no
  worktrees; the row half runs for every backend (that is what makes it measurable on a hub), the name half only
  for `Backend::Docker`.
- **The gate's two cases own their state.** `case_reconcile` kills `-9` a *real* turn (the stub emits a frame
  every 200 ms and the prompt's 202 moves the row to `running` before the first one, so the kill lands mid-turn)
  and a real provision (four 200 ms steps); `case_reconcile_orphans` needs the daemon and the image and sets
  `docker_second_session_id` to the fake id, so a `fail` in the middle leaves neither the orphan container nor
  its network behind.

### Landmines: the doctor, the ports and a read-only open (HS-045)

- **A test's fixture must be written *after* the base value it overrides, and the evaluator is not your friend.**
  `Tools { tailscale: shim(&dir, body), ..base(&dir) }` — where `base` writes the *same* shim path with its own
  body — passed the Tailscale arms while asserting the other arm's answer. Struct-update syntax's evaluation
  order is a detail no test's subject should depend on: build the base, then assign the field. The same shape
  bit a test that reused one `git` shim for two different versions.
- **A fake daemon socket that answers once is right for one check and wrong for one command.** `tests/preflight.rs`
  answers a single `_ping` on purpose (a listener that accepted twice would hide a retry); `doctor` asks once *per
  run*, and the tests drive several runs over one machine, so its fixture loops. The gate's `fake_docker_ping`
  already loops, which is why `case_doctor_quick`'s three runs share one socket.
- **A listener that drops after one accept makes a held port free.** The `--full` arm asserts that a port in use
  plus a `200` from `/api/health` is a *pass*; a health server that answered one request and left had released
  the port by the time the port check bound it, so the assertion read `is free` and looked like the pass it
  wanted. A whole-run server is a thread with a stop flag, and it is the only fixture that measures both halves.
- **`--data-dir` is a `serve` override, not a config key.** A case that runs `doctor --config <file>` must put
  `data_root` *in the file*, or the doctor reads `/var/lib/holosmith` — on this machine a real directory. That is
  how `case_schema_newer`'s new doctor half was written (and `plan`'s first draft had it wrong).
- **A read-only SQLite open of a WAL database needs the `-shm` file, i.e. write access to the data root's
  directory.** `db::recorded_without_migrating` opens with `SQLITE_OPEN_READ_ONLY` and works because `doctor` runs
  as the user that owns the data root; that is also why it is a second opener rather than `Database::open`, which
  migrates before it answers.
- **`git merge-tree --write-tree` answers in two shapes**: the clean merge is exit 0 and exactly one line (the
  tree's id); the conflict is exit 1 and, after the tree's id, `<mode> <oid> <stage>\t<path>` lines. Measured on
  2.43. The trial asserts both — a shim that exits 0 for both is what a first draft's "kind of works" git did, and
  the check has to fail on it.
- **`doctor --full`'s proxy lines pass only at item 10 §7's moment.** `serve` starts `hs_egress`/`hs_key` on every
  Docker boot and nothing stops them, so a `--full` run against a machine that is not serving reports both as
  failures — correctly. The gate's own case therefore boots its own Docker `serve` first, and a demo has to do the
  same or paste a failure it did not mean. Leave the proxies of *someone else's* server alone; remove the ones
  your own demo started by name.
