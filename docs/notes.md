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
   one `fired` so the first to fire owns the reason and no second climb is spawned. The seven reason words
   for a climbed stop are `forced_abort`, `deleted`, `evicted`, `idle_stop`, `turn_timeout`,
   `budget_exceeded` and `disk_quota`; an eighth, `pi_exit`, is not a climb at all but the *clean exit*
   HS-030 records on a `stopped` row (fact 17).
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
    V4 and V5 pass against a real `opencode-go` key, and the spec's verification table now has a **Result**
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
  | `error` | `{"code", "message", …}` — **no message id**; a machine policy ending the session, first the disk cap (HS-063): `code` is the row's reason word, `size_kib`/`limit_gb` are the measurement |
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
  `stoppedNotice` is now **three words wide** (`evicted` "to make room", `idle_stop` "sitting idle", and
  `disk_quota` "using too much disk" — HS-063), each with its own test and none shared with `forced_abort`,
  `deleted` or `turn_timeout` — a stopped row for those keeps the dash, and `turn_timeout` is resumed at once
  anyway so it shows as `idle · turn_timeout`. Adding a word means a sentence *and* a test that it is not
  another's. `error` is also the platform's own `EventSource` event name, so the page's one listener guards on
  `instanceof MessageEvent`: a connection failure is not a frame.
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

- **A large body in a shell variable kills the gate with no failing assertion — and it has now happened twice
  (HS-033, HS-053).** `sed '/^\r$/q'` and `grep -q` quit at the first match while the writer is still writing, so a
  256 KB `$(cat …)` leaves `printf` with SIGPIPE and `set -o pipefail` exits the run **141**, printing only
  `work dir kept:` — no `FAIL` line at all. HS-033 moved the call sites to reading the *header block* off the saved
  file; HS-053 made `expect_header` and `expect_no_cors` cut with bash's own `${response%%$'\r\n\r\n'*}`, which
  cannot be interrupted, so a call site no longer has to know. The same trap inside an **assertion** is quieter
  still: `printf '%s\n' "$body" | grep -qF needle` reports **141 when the needle matched**, so a passing check
  prints a `FAIL` (`case_changes_diff`'s first run did exactly that, after the same SIGPIPE had already killed the
  run once). Read the saved `.body` file with `grep -qF needle "$file"` instead of piping a large variable, and
  keep `printf`-fed pipelines for the answers that are a few hundred bytes.
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
- **`doctor --full`'s proxy lines pass only at item 10 §7's moment, and "not serving" has two shapes.** `serve`
  starts `hs_egress`/`hs_key` with `--restart unless-stopped` and nothing stops them, so a machine whose server
  *stopped* still reads two `proxy: pass` (the containers are running), while a machine that has **never** served
  reads two failures — correctly, because item 10 §7 runs `--full` right after a deploy. The gate's case and any
  demo therefore boot their own Docker `serve` first, or they paste a failure they did not mean. Leave the proxies
  of *someone else's* server alone; remove the ones your own demo started by name.
- **`std::net::TcpListener::bind` sets `SO_REUSEADDR`, so doctor and serve agree about a TIME_WAIT port** — measured
  for HS-045's review, which suspected the two binders differed: a std bind over a port the server had just released
  (one connection made and closed server-side, so `(port, peer)` sat in TIME_WAIT) succeeded, and a *live* listener on
  the same port still answered `AddrInUse`. So "in use" in `doctor`'s port check means a listener, not a leftover
  socket, and a restart `serve` would bind is never refused.

### Landmines: the verification run (HS-046)

- **The spec's `Result` column is the only record of a verification row** (ADR-0055). Do not write a
  `docs/verification/…` report beside it: the cells already carry the verdict, the automation and the fallback,
  and `case_verification_table` reads *those*. The cell is the row's **last** field, so a `|` inside one
  truncates it: the gate fails loudly with a confusing message rather than passing — write around it anyway.
- **A cell names something the repository can run**, and the case checks that it exists: a `case_*` must be in
  `serve_cases`, a `tests/*.rs` is resolved under `crates/holosmith/`, a `scripts/*.sh` and `docs/build.md` at the
  repo root. So renaming a case is a two-file change (the script *and* the cell it fills), which is the point.
- **The gate prints one `ok <case>` line per case that measured something since HS-046.** Before that a green run
  could say how many cases passed but not which, so a demo could not show the case a row rests on. The line is
  withheld when the case printed `SKIP` — a case's stdout is captured for exactly that question — because an `ok`
  for a skipped case is the P1 shape this gate may not have on V6 and V7, whose cases skip a daemon-gated half.
  `ok serve: N cases` is the phase's last line, `grep -c '^ok case_'` counts the ones that measured something,
  and a `SKIP n of N cases measured nothing` line appears when the two differ.
- **This story did not re-measure V3–V5: there is no provider key anywhere on this machine.**
  `/var/lib/holosmith` does not exist and the gate stores none (`case_key_proxy`'s store is its own work dir), so
  their cells quote HS-040's recorded by-hand run and say so plainly ("not re-measured in this story"); a cell that
  only said "re-run from `docs/build.md`" would read as if this story had. A session that wants them *measured*
  again has to mint a key first — and must not put it anywhere a gate can read.
- **A verification row is not a `doctor` check** unless it needs neither a session nor a key. V2 is one; V1 needs a
  container, an image and a scratch repository (its case spells the four mounts as the spec does, on purpose, and
  `tests/container.rs` asserts the binary's vector), V6/V7/V9 need a session, V8 is `cargo test`'s fixture replay
  and V10 is the build. "`doctor --full` automates as many as it can" is a per-row fact, now recorded per
  row, not a licence to widen `doctor` until it starts containers.
- **`docker network ls | grep -x egress` never matches** (the resume checklist carried that line): the output is
  `ID NAME DRIVER SCOPE`, so the row is `4ef0ec3fa08a   egress   bridge   local`. `grep -w egress` is the probe; the
  written form failed on a machine that *had* the network, which reads as "the machine is missing it".
- **A Docker `serve` gate run leaves `hs_egress`/`hs_key` running** (HS-037's note, hit again here): they are one
  line of `docker ps` and the next `tests/container.rs` run skips its real egress path while they are up. Remove
  them by name before a change that touches the proxy.

### Landmines: the deploy, the ports and what `install` does to a key file (HS-047)

- **`doctor --quick` fails a held port on purpose, and `deploy` is the one caller that must not.** `--quick`'s two
  moments are *before a start* and, at `--full`, *after one* — so a held port is a failure at `--quick` and a pass
  only when this machine's own `/api/health` answers. But the spec promises `server deploy` is safe to re-run, and a
  re-run happens on a machine that is already serving. So step 1 downgrades **exactly one** finding, `ports`, to a
  remark, and every other `fail` still stops the deploy. Do not "fix" that by widening `doctor` (its exit code is
  `ExecStartPre`'s) and do not widen the exception: a non-`ports` failure must still stop the run, and
  `tests/deploy.rs` has a test for each half.
- **`install` truncates.** `keys.toml` holds every stored provider key, so step 2 creates it only when it is
  absent (`install -m 0600` from a staged empty file) and repairs a wrong mode with `chmod`. An `install` over an
  existing key file silently destroys the operator's keys and leaves a file that still passes every mode
  assertion — this is the one mistake in step 2 that a demo cannot show.
- **`--root` prefixes what the deploy *installs*, not what it *reads about the machine*.** `data_root` and
  `projects_root` are prefixed (and the `config.toml` it writes carries the prefixed values); the docker socket,
  `doctor::SYSTEMD_DIR`, the listen addresses and step 4's binary mount source are not. And `deploy::run` takes the
  **already prefixed** `Config` — pass the unprefixed one and every path is prefixed twice (`<root><root>/var/…`),
  which still creates directories and still looks installed.
- **Step 4 reads readiness, not the allow-list.** Its read is the `egress` network plus both containers' `running`,
  because a changed `[egress] allow` is an *argument* and takes effect on the next `serve` boot, which replaces both
  proxies unconditionally (ADR-0037). Comparing `docker inspect`'s `Args` against `run_args` was rejected: it is a
  second reading of one vector and it drifts.
- **Step 7 cannot be `doctor --full`, and not because it is slow.** `--full` asserts the image and the proxies step 3
  builds, so on a machine where step 3 is skipped the last step would fail for a reason another story owns. The
  first draft of the plan waited for `/api/health` against a server the *gate* had booted — a measurement of the
  harness. Steps 5–7 are HS-049's, and the URL and next steps are the command's tail. **Amended by ADR-0058:** HS-049
  wrote step 7 as exactly that — the `/api/health` wait and `doctor --full`, in process — and the objection does not
  survive, because step 3 is no longer skipped: the image and the proxies the last step asserts are the ones the same
  run just built and started.
- **A run whose remaining steps belong to later stories exits 0.** The exit code is 1 iff a step *failed*; the
  summary's `deploy: incomplete: 4 of 7 steps …` is what says the install is not finished. A script that reads only
  the exit code will believe a partial install succeeded — that is deliberate (the acceptance's failure semantics
  are about a failing step) and it is why the line is always printed, not only when steps were skipped.
  **Amended by ADR-0058:** nothing is skipped any more, so the `incomplete` line is deleted (not hidden) and the
  tally prints `skipped` as zero. The exit code is still 1 iff a step failed.
- **The gate's case takes over the machine's `hs_egress`/`hs_key` names.** A leftover proxy from `case_doctor_full`
  or from a `serve` boot makes this case's own first run read `satisfied` for step 4, so it `rm -f`s both before it
  starts and after it finishes. `case_doctor_full` deliberately leaves its own running, so the order in
  `serve_cases` matters: `case_server_deploy` runs before it.
- **`find | stat '%a %s'` is a listing, not a hash, and the pair of `cmp`s is not the whole answer either.** The
  listing catches a changed mode, size or set of names; the `cmp`s catch a rewrite that keeps all three; and a
  re-install of *identical* bytes passes both — which is why the case also greps `satisfied` on steps 1, 2 and 4.
  Three detectors for one claim, each blind to what the others see: delete any of them and "the second run changes
  nothing" becomes a claim nothing measures.
- **Step 1's `disk` finding is `unknown` on the run that matters.** `preflight::disk` answers `unknown` when the
  data root does not exist (its own sentence says `serve` creates one at startup), and on a fresh host that is
  *every* first deploy — so the floor `min_free_gb` guards would be first measured by the second run, after step 2
  has already created the tree. `deploy::preflight` therefore re-asks the same check against the nearest existing
  ancestor and puts that path in the sentence. Do not "simplify" it back to the report `doctor` returns, and do
  not widen `doctor` itself: its `unknown` is right for a machine that has not served yet.
- **`sudo` needs the deploy's stdin.** `Command::output()` gives the child `/dev/null`, so a `sudo install` on a
  machine whose credential is not cached fails with `a terminal is required to read the password` — a deploy that
  can only work as root or with NOPASSWD. The child inherits stdin (and only stdin: its stdout and stderr are
  captured for the failure's sentence).

### Landmines: the image builds (HS-048)

- **The build context is a tar on stdin, and that is not a style choice.** `docker build -` from an archive
  holding `Dockerfile` and the running binary is the only way to build without a *path* crossing into the
  daemon's namespace. A context directory under `target/` would be resolved in the **host's** namespace, so a
  daemon that cannot see it builds from an empty root-owned directory and the failure arrives later as a
  `COPY`/`RUN` error inside the image — the same landmine as the data root (ADR-0039) and the proxy's binary
  mount (ADR-0045), and the reason the sandbox (whose `/tmp` is tmpfs) can measure the honest form but not the
  other. `images::archive` writes ustar by hand with `uid`, `gid` and `mtime` **zero**, so the same two files
  always produce the same archive bytes; a `mtime` of "now" would make every build a different context for
  the daemon's cache.
- **A build's output must not go to a pipe.** A build prints hundreds of lines; a pipe nobody drains fills at
  64 KiB and the build deadlocks, and `Command::output()` only helps after the build is over. `build_bounded`
  sends stdout **and stderr** to a temporary file, writes the archive from a spawned thread (a deadline cannot
  fire while the call is blocked in `write`), polls `try_wait` to the bound, kills on expiry and reads the
  file back. `Ok(None)` is "did not finish", not an error with a status.
- **A failed `docker build -t <tag>` does not create the tag — and does not move it either.** Measured: after
  a failed build `docker image inspect <tag>` answers "no such image" *when the tag was new*. If the tag
  already pointed at an older image, that image stays; so an absence check is not a skip rule and a failed
  rebuild is not "the image is gone". That is also why the failure sentence says **no new image was tagged**
  rather than "nothing was tagged": the second sentence is false in the one state a rebuild actually produces.
  The skip rule is the labels, which is also why the deploy's read never treats "the daemon could not answer"
  as "not built" in anything but the plan's direction.
- **The base digests are manifest-list digests, and `docker manifest inspect -v` does not contain them.** The
  `FROM` line records the index digest (`alpine:3` → `sha256:3f4d6b32…`, `node:24-bookworm-slim` →
  `sha256:01d566e8…`), which is what makes one Dockerfile resolve to the machine's own architecture. Checking
  the pin by hand with `docker manifest inspect -v` shows the per-platform digests, not the list's — the
  values are right and the check is the wrong one.
- **The proxy image cannot be `FROM scratch`.** `hs_key` runs `openssl s_client` (ADR-0048), so a proxy image
  without `openssl` (and `ca-certificates`) turns every model call into a 502 — while looking perfectly healthy
  in `docker ps`. `alpine:3` + `apk add --no-cache openssl ca-certificates` is the narrow answer and is
  measured serving both `proxy egress` and `proxy key` with no mount.
- **The agent image's base is node because pi is a node CLI.** Measured: `/usr/local/bin/pi` is a symlink into
  the npm global tree, so a base without node is exit 127 at launch (HS-036's lesson). `node:24-bookworm-slim`
  carries node 24 and the `node` user at uid 1000, which the session's container needs to stay the host user's.
- **After this story a `serve` boot cannot start a proxy without the proxy image.** `proxies::start_*` asks
  `Containers::image_exists` and warns with a sentence naming `holosmith server deploy` step 3; the boot still
  succeeds, and a session that allows internet access reaches nothing. That is also why `case_server_images`
  runs **first among the cases whose boot starts the proxies** (not the first daemon case — `case_create_session`
  and friends precede it) and keeps both images: every later case whose boot starts the proxies depends on them,
  and a case that removed them at the end would break the suite in a way that looks like a different bug. Its
  "absent state" is **asserted, not assumed**: a running container pins the image it was started from, so
  `docker image rm` fails while `hs_egress` is up — an interrupted earlier run is the normal cause — and the
  build that follows would be a no-op inside a case that claims to have measured a build. It removes the deploy's
  own two containers first and then checks `docker image inspect` answers "no such image" before deploying. `HOLOSMITH_TEST_IMAGE` (`pi-agent:local`) still serves the cases that are not about images,
  and it is a valid `agent_image` — `doctor`'s `image` finding says once that it carries no Holosmith labels
  rather than failing, because a hand-built image is a supported machine.
- **Two of the five labels are skip rules, not decoration.** `org.holosmith.toolchains` on the agent image is
  what makes a changed `agent_toolchains` a rebuild: without it every label would still match, and the deploy
  would answer `satisfied` for an image whose contents no longer match the config. `org.holosmith.binary-sha256`
  is the same argument for the artifact itself, and it was **the review's P1**: `git describe --tags --always
  --dirty` is identical for two builds of the same commit — and for two builds of the same *dirty tree*, which
  is what every development machine has — so version + tag + pi compared equal over an image holding the
  previous binary, while `hs_egress` kept serving it. Do not "simplify" the read back to three strings: the
  gate cannot catch that, because it deletes the tags to force a build. The SHA-256 is written out in
  `images.rs` (no dependency for it) so an operator can check the label with `sha256sum` on the artifact. The
  same reason `config/schema.rs` counts keys: `agent_toolchains` moved `KNOWN_KEYS.len()` to 33 and the golden
  file with it.
- **An empty `agent_toolchains` list must not rebuild, and a missing label is not a mismatch for it.** An empty
  list is a legitimate machine (git, openssl and CA certificates only), so `--build-arg HOLOSMITH_TOOLCHAINS=`
  gives the Dockerfile an empty value; whether a `LABEL key=""` survives in `Config.Labels` is the daemon's
  business, so `Expectation::mismatches` accepts an absent toolchain label when this binary's list is empty.
  Without that, an empty-list machine would rebuild on every deploy and fail `doctor` forever.
- **The key proxy runs as the key store's owner, and that is not tuning.** The store is a 0700 directory and a
  0600 file belonging to the user `serve` runs as, and the container is `--cap-drop ALL`, which takes root's
  `DAC_OVERRIDE` with it — a container running as uid 0 cannot read the file, exits 1 and `--restart
  unless-stopped` loops it. HS-048's first gate run measured exactly that; the old proxy image only worked
  because its base's user happened to be uid 1000, which is an accident, not an identity. `KeyProxy::run_args`
  takes `--user` from the key **file**'s owner, and `proxies::start_key` **refuses** when the file's owner and
  the mounted directory's owner disagree, because one `--user` cannot satisfy both and no flag is right then.
  The gate asserts the uid equals the *invoking* user's and is not zero — 1000 is only this sandbox's spelling
  of ADR-0035's rule.
- **The architecture guard exists because `docker_host` may be another machine.** `COPY holosmith` puts the
  *running* artifact into the image, so a daemon building for a different architecture produces an image whose
  entrypoint cannot start — a failure that would surface as a proxy that will not run, minutes later. The
  apply compares `{{.Server.Arch}}` (`amd64`/`arm64`) with `HOLOSMITH_TARGET`'s prefix once, before the first
  build. The **read** does not ask: a dry run must remain a plan on a machine whose daemon cannot answer.
- **The image builds are large, and `doctor`'s disk finding measures the same filesystem.** Two agent-image
  builds, a pair of musl artifacts and a long-lived debug `target/` left this machine at 979 MB free, and
  `case_doctor_quick` failed with `disk: fail: … has 0 GB free and min_free_gb is 1` — a true finding about the
  machine, not a flaky case. `cargo clean` and `docker builder prune -f` freed 17 GB. Leave headroom before
  `scripts/verify.sh`; do not "fix" the case or the floor.
- **Breaking a Dockerfile to test the failure needs the embedded const, not a copy.** The Dockerfiles are
  `include_str!`ed, so a gate case could only corrupt a copy on disk and would prove nothing. That arm lives in
  `tests/images.rs`, drives the production `Images` path into a **test-only** tag and asserts both halves:
  the sentence names the instruction and the layer, and the tag does not exist afterwards.

### Landmines: the service, the keys and the origins (HS-049)

- **`openssl ecparam … -text` prints the parameters, not the numbers.** The private scalar and the public point
  are in the DER, not in `-text`'s dump, so the generation child is `openssl ecparam -name prime256v1 -genkey
  -noout -outform DER` and what it writes on stdout is a 121-byte SEC1 key: `30 77 02 01 01 04 20 <32-byte scalar>
  a0 0a 06 08 2a8648ce3d030107 a1 44 03 42 00 04 <64-byte X||Y>`. `vapid::from_der` walks exactly that shape and
  refuses anything else with a sentence naming what it expected; the `-text` form would have been a pair
  half-understood, and a wrong signature is a failure only a phone would notice.
- **`tailscale serve status --json` is `ipn.ServeConfig`, and the flags are not optional.** The shape is
  `{"TCP":{"443":{"HTTPS":true}},"Web":{"<host>:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:8080"}}}}}` — the
  `Web` object keyed by `host:port`, with the origin's target behind `Handlers["/"].Proxy`. `--bg` is required or
  the command blocks in the foreground, `--yes` keeps a first `serve` from prompting, and a gate stand-in **must
  read the port from `$4` and the target from `$5`** of `serve --bg --yes --https=<port> <target>`: the first draft
  read `$3`/`$4` and wrote `--yes --https=8444` into its own state, which made every run after the first re-apply
  forever. Same family as ADR-0035's "ask for the field": the JSON is the read, the human text is prose.
- **`systemctl` exists here but exits non-zero, and that is not a spawn error.** Measured: `/usr/bin/systemctl` is
  present and answers `System has not been booted with systemd` because `/run/systemd/system` is absent, so step
  5's failure is the child's own line rather than "could not be run". `useradd` and `usermod` are **absent**, so
  the system path's user, its group and the three `chown`s are `tests/deploy.rs`'s recorders — a gate may not
  create a system user, and a case that tried would only pass as root.
- **A `--root` deploy must be driven with the musl artifact, not `target/debug/holosmith`.** Step 3 copies the
  *running* binary into `alpine:3`, and a glibc debug binary starts with `exec /usr/local/bin/holosmith: no such
  file or directory` (exit 255), so the proxies die seconds later and step 7 fails on `hs_egress is not running` —
  a failure two steps away from its cause. Measured in this story's first demo; use `dist/<triple>/holosmith`.
- **`--root` prefixes `data_root` as the config spells it.** A config that names an absolute `data_root` under
  `target/` gives `<root>/home/…/target/…/data`, not `<root>/var/lib/holosmith`, and both look installed. The
  gate's deploy configs therefore leave `data_root` at its default, so the tree reads `<root>/var/lib/holosmith`
  and a case can name the paths it asserts (ADR-0056).
- **`perms` made the data root's mode a startup finding, so `serve` had to set it.** `create_dir_all` gives the
  process's umask — 0775 under umask 0002 — and `doctor --quick` is the unit's `ExecStartPre`, so a root `serve`
  created would fail the *next* boot over a mode nothing had changed. `commands/serve.rs` now sets 0755 on a root
  **it** created and leaves an operator's own mode alone, and the gate's `doctor` fixtures had to create theirs
  with `install -d -m 0755`; a fixture made with `mkdir -p` is a machine the deploy never leaves.
- **Step 7's read runs `doctor --full`, so a dry run runs the network self-test too.** The price is recorded rather
  than hidden: a plan that read nothing is not a plan (ADR-0056), and the self-test removes the network it makes
  (ADR-0044), so a dry run leaves no state — it spends seconds, and the alternative is a plan describing a machine
  nobody measured.
- **`perms` is a `doctor --quick` finding, so `server deploy`'s step 1 must *downgrade* it.** Without that the deploy
  refuses on a mode it repairs two steps later: step 2's `chmod`/`chown` actions become unreachable and the operator
  is told to run a `chmod` the deploy knows how to run. `serve` still refuses, because `ExecStartPre` is what a wrong
  mode must stop. There are now **two** recorded divergences in step 1 (`ports`, `perms`), each with its own remark,
  and `tests/deploy.rs::an_existing_key_file_is_never_rewritten` fails if the second one is dropped.
- **The tree `perms` reads is the *caller's*, so it comes through `doctor::Tools`.** `config_path` and `admin_dir`
  are fields, defaulting to the machine's `/etc/holosmith/config.toml` and `/run/holosmith`; `deploy::run` overwrites
  them from the `Target`. Spelling them inside the check made a `--root` deploy's step 7 compare the *host's* real
  install against the target's data root — a false failure naming a `chown` on a file the run never wrote, on every
  host that has deployed for real (HS-049's review, SPEC-1; the arm that catches it is
  `tests/doctor.rs::the_installed_paths_modes_and_owners_are_one_finding`'s admin-directory case).
- **Three creators write the data root's mode, so all three write 0755.** `serve`'s boot, `key::Store` (which
  `holosmith key set` calls, the documented next step after a deploy) and `server deploy`'s step 2.
  `create_dir_all` gives the umask's 0775, which on this machine (umask 0002) is what a store created before any
  deploy used to leave behind — a mode no part of the product chose, reported by `perms`. Each creator sets 0755 only
  on a directory *it* created; an operator's own mode is theirs.
- **`grep -qF "--https=…"` is not a search, it is a broken invocation.** The argument starts with `--`, so grep reads
  it as its own long option and exits 2 with `unrecognized option` — the assertion fails while the behaviour is right
  (it cost this story one full gate run). `grep -qF -- "$target" "$log"` is the form, and the trap applies to any
  pattern built from an argv.
- **A case's offline half can be green while its online half's greps are wrong.** The extracted-run trick that keeps
  the gate's ten-minute loop out of a working session only exercises what needs no daemon; an argv grep in the online
  half is therefore untested until the real run, so read those lines twice.
- **`scripts/verify.sh` is bash, not sh.** `sh -n` fails on it at `HEAD` (line 207's `local -a sent=("$@")`), so
  `bash -n scripts/verify.sh` is the cheap check after every edit to it — cheaper than the minutes a full run
  costs, and it catches the quoting mistake a heredoc full of shims invites.

### Landmines: `status`, `logs` and `uninstall` (HS-050)

- **`status` asks the address in the config, and `start_serve` passes `--port 0`.** So a case that wants the uptime
  line measured has to boot its own `serve` *on the config's port* (`case_doctor_full`'s shape, not `start_serve`'s)
  or it will read `uptime unknown:` and quietly test the arm it did not mean. That is also the shape of the mistake
  this bullet exists to prevent: a `status` case against the shared boot would print SKIP-worthy nonsense and still
  pass, because `unknown` is a legal line.
- **The health payload is `Health`'s six keys (the sixth is HS-067's `application_server_key`), and a 200 is not enough.**
  `http::health::ask` parses the body, so a
  test fixture that answers `200` with `{}` is *not* a serving machine any more (HS-045's doctor test had exactly
  that fixture and went red when the reader landed). `doctor::health_answers` is now `ask(...).is_ok()`: one reader,
  and "a port somebody else holds" is a different answer from "this machine's own server answers".
- **`docker rm -f` on an absent name exits non-zero**, so "removed" cannot be read from the child: `uninstall` reads
  **existence** (`docker container inspect`) and then removes. `running` is the wrong read for a teardown —
  `--restart unless-stopped` keeps a stopped container's name and definition.
- **`uninstall --root` asks systemd nothing, so the privileged arm's `disable --now` argv is only reachable where a
  unit file exists.** `tests/uninstall.rs` gets there by making `systemctl is-enabled` answer 0 with no file (a
  linked unit, or one left loaded after a hand removal) — which is a real state, not a test fiction. The gate's case
  is the `--root` arm, and `docs/build.md`'s by-hand section is the privileged one.
- **A privileged teardown must not exec its own `rm`.** `tests/deploy.rs`'s `sudo` recorder exists for the same
  reason and applies here twice over: the argv names `/etc/systemd/system`, and a shim that forwarded to the real
  `sudo` would remove a unit from the machine running the tests.
- **`rm -rf` of a data root is one child, and the purge's names come from the rows.** `s_<id>`/`agents_<id>` are
  derived from an id, never from a `docker ps` listing, so nothing an operator named can be swept — the same rule
  the startup reconcile follows (ADR-0053) from the other direction.
- **The purge refusal is a `Result::Err`, not a failed step**, and the command prints it on stderr with no resume
  line. A report with a `failed` line would invite "re-run the resume command", which would be refused the same way;
  the sentence names `--force`, which is the only thing that changes the answer.
- **`uninstall`'s own three words are not the deploy's five.** `satisfied`/`needs`/`changed`/`skipped` describe a
  read-and-apply step; a teardown's commonest answer is "there was nothing there", which a deploy has no word for.
  `tests/uninstall.rs::the_report_prints_its_target_and_one_line_per_thing` fails if a deploy word reappears.
- **A followed session log is a poll loop, and the test appends on the closure's second call.** That is what makes
  "a line appended after the first read is printed" a measurement with no clock and no `tail`; a loop that read once
  and returned prints the first line and returns, which is exactly the mutation the test catches.
- **A data root under `/tmp` makes the key proxy fail, and the reason is two layers away.** The daemon resolves a
  mount source in the *host's* namespace, so a `-v` of a tmpfs path it cannot see silently creates an empty
  **directory** at the destination — ADR-0035's landmine, hit by HS-050's first demo. `hs_key` then finds
  `<data>/secrets/keys.toml` is not a file, runs `key::Store::ensure_file`, and that `chmod`s a **read-only** mount
  (`docker logs hs_key`: `the key store could not be prepared: Read-only file system`), so the container exits 1 and
  `--restart unless-stopped` loops it — while `status` honestly prints `proxy hs_key not running`. Every demo and
  gate fixture with a data root therefore lives where the daemon can see it (`target/`, `~/.pi/`, the repo), and a
  key proxy that is not up is worth a `docker logs hs_key` before anything else. Not fixed here: the key proxy is
  HS-039/HS-040's, and this story's job was to report it.
- **`--port`/`--data-dir` are `serve` overrides, and `status`/`logs`/`uninstall` read the config.** A demo config
  that names only `control_listen` in the file and passes `--data-dir` to `serve` gets a `logs --session` that reads
  the machine's real `/var/lib/holosmith` — which is how HS-050's first demo printed "no log for session <id>" over
  a session that had one. Two configs is the shape that works: one for the run (`data_root` in the file) and one for
  a `--root` teardown (`data_root` omitted, because `--root` prefixes what the file spells).
- **`Docker::running` answers a `bool`, and every daemon error is folded into `false`.** So a report that asks it
  alone cannot tell "the container is down" from "I could not look" — a socket this user may not open, a daemon that
  is down and an account outside the `docker` group all print `not running`. `preflight::daemon` is the one reader that
  separates the two, and both `doctor`'s proxy findings and `status`'s proxy lines ask it **first** (HS-050's review,
  T1, which is the story's own rule turned on itself). The same shape applies to any future read that returns a
  `bool` over a fallible question: the question's failure has to be answerable separately, or the report invents a
  measurement.
- **`Path::exists()` is `false` for every stat error, not just "not there".** A read-only opener that answers "no
  file" over a permission error turns an unreadable data root into "this machine has never served" — and one caller
  decides whether to *delete* the tree over that answer. `db::Reader::open` matches `ErrorKind::NotFound` and returns
  the error for anything else (HS-050's review, T2).

### Landmines: the drain and its socket (HS-051)

- **`serve`'s default admin directory is the machine's real `/run/holosmith`, and in this sandbox an unprivileged
  user can write it — so a test that forgets `--admin-dir` binds a real socket silently.** This story's first
  handoff had eight such spawns (`tests/db_migrations.rs`'s three, two direct ones in `tests/serve_cli.rs`, three of
  `verify.sh`'s own boots; `start_serve` already had the flag, which is why the mistake stayed invisible in the case
  that uses it most) and the first `cargo test` after them left `/run/holosmith/drain.sock` behind, which then
  failed `tests/deploy.rs`'s `perms` check with `/run/holosmith is 0775 where server deploy writes 0700` — a
  leftover from an *earlier* session's by-hand `mkdir -p /run/holosmith` (eight of them are in the operator's log)
  and a failure with no relationship to the code under test. Two rules came out of it: every boot in a test or a
  gate passes `--admin-dir` (the shim is `Server::start` and the gate's `start_serve`, both under `$WORK`), and a
  red `tests/deploy.rs` `perms` failure is worth a `ls -ld /run/holosmith` before anything else.
- **`UnixListener::bind` answers `AddrInUse` for a socket file whether a server is behind it or not, and `connect`
  answers `ECONNREFUSED` for a *regular file* too (measured).** So "unlink the stale socket" cannot be a `connect`
  test alone: the file's type is checked first (`symlink_metadata` + `FileTypeExt::is_socket`), a socket is probed
  (refused → dead → unlink and bind; connected → another server owns it → `warn` and serve without one), and
  anything that is not a socket is an operator's file that is named and left alone.
- **A drain test that "waits for the turn" passes by finishing at once unless the deadline is under one frame.** The
  stub's turn is twelve 200 ms frames (~2.4 s), so `--wait 200ms` is what makes the expiry arm a measurement rather
  than a race;
  the same shape applies to `case_drain_restart`, which needs the turn still `running` when the `kill -9` lands.
- **A read timeout on a protocol that is *silent between facts* is not a dead connection.** `drain`'s socket writes a
  line when the count changes and nothing in between, so a turn that takes a minute produces two lines and then a
  minute of silence; the client's `ANSWER_TIMEOUT` is a `SO_RCVTIMEO`, and treating its first expiry as "the server
  stopped answering" made `holosmith drain --wait 1m` fail after five seconds against exactly the turn it exists to
  protect. The rule is the one `commands/drain.rs::ask_with` now follows: bound the **first** line (is anything there
  at all), then tolerate silence up to the deadline the operator asked for. Any future streaming client of a
  progress-line protocol has the same trap, and the gate cannot see it because every fake socket in
  `tests/drain_cli.rs` wrote all its lines before the client read.
- **`steer` refuses unless the row is `running`** (`NOT_RUNNING_STEER`), which is the whole reason a draining machine
  can keep answering it: it cannot start work, only shape a turn that is already ending. Any future handler that
  *can* start work belongs in the refused list beside create/prompt/resume, not beside steer.
- **A resumed session is refused a prompt while draining, so the UI must not answer a drain with an
  "resume and retry" affordance.** The `503 draining` sentence names the drain and the `--wait` for exactly that
  reason: the operator's move is to wait or to restart, not to provision.
- **The prior session left this tree dirty and unformatted, with no commit and no Linear progress past the plan.**
  What made it adoptable was that the recorded plan comment matched the code (module shape, wire protocol, refusal
  list, the four reason words) and that everything was already compiling — but `cargo fmt --check` was red and an
  unrelated `Uninstall` help-text line had been shortened to `Uninstall.` on the way past. A handoff brief says "stop
  green"; a killed session cannot, so the *next* session checks `git status`, the Linear plan and `cargo fmt` before
  believing a dirty tree.

### Landmines: the changes view (HS-052)

- **A symmetric fixture cannot measure a direction, and the first version of this story's own proof could not tell
  `behind` from `ahead`.** `the_summary_names_every_field_and_counts_both_sides` had one commit on the branch and one
  on the target, so both counts were `1`; mutating `behind`'s range from `<branch>..<target>` to `<target>..<branch>`
  left it **green**. The bullet it exists for — "behind counts commits on the target the branch lacks" — was unmeasured
  until the fixture gained a second branch commit (2 and 1), and the gate's `case_changes` had the identical shape and
  got the identical fix plus an explicit `counts_differ` assertion. Any pair of counts that are "each way" needs
  unequal numbers in the fixture, and the only reliable proof is to mutate the code and watch the test go red.
- **`git diff --numstat --name-status` together prints only the name-status** (measured) — the two reads are two
  commands, and they cannot disagree because both name the same two commit ids (objects are content-addressed). The
  `-z` shapes are `<additions>\t<deletions>\t<path>\0` for numstat (with `-` for a count git cannot make) and
  `<status>\0<path>\0` for name-status; with `--no-renames` the status is always one letter, so a reader never has to
  parse an `R100\0old\0new` triple.
- **`status.showUntrackedFiles = no` is a repository setting that makes `dirty` lie**, and a local `git config` is
  exactly what a test can write without `unsafe`. `a_repository_config_cannot_hide_an_untracked_file` sets it and
  asserts the file still counts; deleting `--untracked-files=normal` from `changes::dirty` turns that test red. The
  same class as HS-025's `worktree list --porcelain` and HS-026's `CONFIG_READ`: a read that inherits config is not a
  measurement.
- **`merge-tree --write-tree`'s two shapes, measured on 2.43:** exit 0 prints one line (the tree's id); exit 1 prints
  the tree's id, then `<mode> <oid> <stage>\t<path>` per conflicted path per stage, then a blank line and git's prose.
  A reader that took every line after the first would report `Auto-merging f.txt` as a path, so the walk stops at the
  first line without a tab (the unit test pins both). Exit 1 is a **measurement** (`mergeable: false`), not a failure
  to run the command.
- **The provisioner takes the worktree back when a later step fails (ADR-0029), so a gate case that wants a real
  worktree with no daemon must re-make it with the step's own command.** `case_changes` boots `backend = "docker"`
  against a `_ping`-only socket (the `case_worktree` trick): the worktree step runs and writes the row, the network
  step fails and the provisioner removes the directory and `agent/<id>`. The case then runs `git worktree add -b
  agent/<id> <absolute path> <base>` itself — absolute, because a relative path resolves against the repository
  (HS-025) — which is a fixture, not a re-implementation of the step.
- **The handler builds the worktree from the naming rule, not the row's recorded path.** `Worktree::of` derives the
  branch, the repository and the directory from the same three facts, so a row whose path is stale and a rule that
  disagrees cannot produce two spellings of one worktree; `Changes::read` then refuses a directory git does not
  register rather than trusting either.
- **A predicate with two arms must apply each arm to its own kind of entry.** `changes::touches_ci` compares
  `CI_PATHS` entries that are directory prefixes (ending `/`) against the whole path and entries that are file
  names against the last segment; the first version applied *both* tests to *every* entry, so a root-level
  `Jenkinsfile.old` or `.gitlab-ci.yml.bak` answered `true` through `path.starts_with`. The test that was meant to
  catch look-alikes used `src/Jenkinsfile.rs` — whose path does not start at the entry, so it passed over the
  root-level case — which is the same lesson as the symmetric-count fixture: a test that picks the one input where
  the bug is invisible is worse than no test. The review found it; the fix is to branch on `entry.ends_with('/')`.
- **A prior session left this tree dirty with the implementation half done and no docs.** What made it adoptable was
  that the Linear plan comment matched the code field-for-field and the code compiled; what it cost was a re-read of
  every line (and the mutation pass above, which found the `behind` gap). A dirty tree from a killed session is not
  evidence of anything on its own — read it, do not inherit its claims.

### Landmines: the unified diff (HS-053)

- **A repository's *local* config is a test's to write, so `--no-ext-diff`, `--no-textconv` and `--no-color` are
  measurable rather than merely pinned — and HS-052's comment claiming otherwise was wrong.** `git config
  diff.external /path/script` in `.git/config` makes a plain `git diff` print whatever that program prints
  (measured: the whole answer became `EXTERNAL DIFF RAN` on one line for one file); a `diff.‹driver›.textconv`
  plus a `*.dat diff=‹driver›` line in `.gitattributes` turns a NUL-bearing binary file's summary into a diff of
  the filter's output; `color.ui = always` makes a *piped* diff carry ANSI escapes (`033 [ 1 m` before
  `diff --git`). The three `tests/changes.rs` tests `a_repository_config_cannot_{run_a_program, make_a_binary_file_
  look_textual, colour}_the_patch` set each one and each goes red when its flag is dropped from the vector —
  which is a stronger proof than the pinned argument list alone, and the reason the previous story's "a
  behavioural test cannot set `diff.external` (the crate denies `unsafe`)" is a lesson: `unsafe` is what a test
  cannot use, and *config files* are what it can.
- **git's binary summary is free and complete: `Binary files /dev/null and b/bin.dat differ` for an added file,
  `Binary files a/x and b/x differ` for a modified one, and not one NUL on stdout.** So "binary files are
  summarised, not emitted" needs no code of ours — only the `--no-textconv` that keeps a repository from
  overriding it. `path=` naming a binary file is a `200` with that line; a `4xx` would say the file is not in
  the changes, which is false.
- **The marker's own length has to be inside the cap, and a marker that names what was *shown* cannot be.**
  `body.len() <= 256 KB` is the acceptance, so the budget for the diff is `DIFF_CAP - marker.len() - 2` and the
  marker is assembled first. A marker like `[showed N of M bytes]` would make N depend on the cut, the cut on the
  marker's length, and the loop either drops a digit or overshoots the cap — so this marker names the **total**
  and leaves "how much is here" to whoever measures the body. Mutating the budget to `DIFF_CAP + 2` turns both
  cap tests red.
- **A cut is floored twice, and the order is a decision.** First to the last `\n` in the budget (a diff is
  line-oriented; half a hunk line misrepresents content), then to a UTF-8 boundary **only when
  `Utf8Error::error_len()` is `None`** — that `None` is what distinguishes "the cut fell inside a character" from
  "these bytes are not UTF-8", and the second must be reported rather than repaired. Dropping the line-boundary
  floor turns the module's cap test red (the wire test cannot see it: it only asserts the marker).
- **A bounded read of stdout must drain stderr on another thread.** Reading stdout in 64 KiB chunks while the
  child's stderr pipe fills is a deadlock the moment git says more than a pipe's worth — `docker.rs`'s `bounded`
  gets away with reading both at the end because `wait_with_output` reads them concurrently; this one spawns the
  drain. And after a kill the exit status is **not** a verdict: the ceiling is ours, so `complete == false`
  suppresses the "git exited N" error that would otherwise turn a deliberately cut diff into a `500`.
- **Counting is what makes the marker honest, so the ceiling is a number and not "stop reading".** Keep
  `DIFF_CAP`, count up to `COUNT_MAX` (8 MiB) and then kill: everything under the ceiling gets an exact total (the
  demo's 2 MB and the gate's 520 KB included), and a monstrous diff gets "at least N bytes" instead of a number
  that stopped being measured. "At least" rather than "longer than", because the loop stops *at* the ceiling
  without asking whether anything follows — a diff of exactly 8 MiB is the case that tells the two words apart.
- **A member of `files[]` is a name, not a pathspec.** A git pathspec is a glob by default and `:x` is magic, so
  `patch_arguments` leads with `--literal-pathspecs`: a changed file literally called `a*.md` would otherwise hand
  git a pattern and get back the union of every match (measured — dropping the flag makes
  `a_name_that_looks_like_a_pattern_is_the_name_and_not_a_pattern` red by returning `abc.md`'s diff too).
- **`sub/../a.txt` is the case that proves membership rather than normalisation.** It resolves to a file the
  branch *did* change, so an implementation that joined the path (or let git resolve it) would return a patch;
  the changed-file list does not contain the string, so the answer is the same `404` as an unchanged file. The
  gate asserts `..%2Fetc%2Fpasswd`, `%2Fetc%2Fpasswd` and a file the base holds, plus "no `root:` in the body" —
  a leak is a `200` with a file in it, which a status-only assertion would pass over.
- **`support::git` trims stdout, so a byte-for-byte comparison of a patch needs its own helper.** `tests/changes.rs`
  adds `git_bytes` (untrimmed, with the production flags) rather than comparing against the module's own output;
  the gate's `case_changes_diff` does the same with `git diff … > file` and `cmp -s`, because `fetch`'s `sed`
  strips only the header block and leaves the body's bytes alone.

### Landmines: the fast-forward merge (HS-054)

- **A dirtiness *pre-check* is the wrong answer, and the wrongness is measurable.** `409 target_dirty` is "the
  checkout has local work the fast-forward would overwrite", and the obvious implementation reads `git status
  --porcelain` in that checkout *before* merging. An **unrelated** dirty file is not that: git carries it across a
  fast-forward and the merge succeeds. So the merge runs first and only a non-zero exit is classified, by
  re-measuring two facts — the ancestry (the target moved → `not_fast_forward`) and `changes::dirty` in that
  checkout (`target_dirty`) — and nothing else. `an_unrelated_dirty_file_does_not_block_the_merge` is the test:
  adding the pre-check back turns exactly that one red (measured).
- **`git merge --ff-only` aborts without touching anything when the tree is dirty**, which is what makes the
  classify-afterwards shape safe: the "would be overwritten" refusal leaves both the ref and the operator's files
  exactly as they were. A **dirty *untracked* file the branch adds** is the same refusal (`The following untracked
  working tree files would be overwritten`), which is the `case_merge` step 4 fixture.
- **`git worktree list --porcelain` prints one block per checkout — `worktree <path>`, `HEAD <sha>`, then
  `branch refs/heads/<name>` *or* `detached` — and a detached checkout has no `branch` line at all.** That is what
  makes rule 4's unchecked-out path reachable in a repository that is always "checked out" somewhere: detach the
  project's own checkout (`git checkout --detach <base>`) and `main` is then a ref no working tree holds. Both
  update paths are asserted separately for this reason, and they differ in *what moves*: the CAS moves the ref and
  not the checkout's files, `merge --ff-only` moves both.
- **`git update-ref <ref> <new> <old>`'s third argument is the whole race-free half of rule 4**, and its *failure*
  arm cannot be provoked from a test without a race. So the argument vector is what a unit test pins
  (`update_ref_arguments`: three arguments, new before old); dropping `old` makes that test red while the
  unchecked-out integration test stays green — which is why the vector exists as a test rather than a comment.
- **A `post-merge` hook is the one hook a fast-forward can run, and it only runs when the merge advanced.** So a
  hook test must commit on the branch first: with "Already up to date" git runs nothing and the test would pass
  for the wrong reason. `a_hook_does_not_run` writes a real `.git/hooks/post-merge` that touches a marker, and
  dropping `-c core.hooksPath=/dev/null` from `merge_arguments` makes it red (measured).
- **A second merge of the same session is a `200` no-op, and asserting that means asserting the reflog too.** After
  the first merge the target *is* the branch's tip, so the ancestry holds and git answers "Already up to date";
  the test checks the same `merged_commit`, an unchanged `status --porcelain` **and** an unchanged
  `git reflog show refs/heads/main`, because a needless `update-ref` would leave the tree identical and still have
  written the ref.
- **The per-project lock's effect is not observable in an outcome.** Without `merge::Merges`, two concurrent
  merges of two sessions would both pass the ancestry test and one would lose the compare-and-swap — the same
  visible result. So what a test measures is the lock's *identity* (one repository answers one mutex, two
  repositories two; a fresh mutex per call goes red), and the ADR says plainly that the CAS is what actually
  protects the ref.
- **`case_merge` walks one repository through both update paths and both refusals**, which is why it moves `main`
  back and forth with `git checkout --detach` / `git checkout main` rather than creating a second project: the two
  paths are the same repository in two shapes, and a second repository would not prove they differ. It re-makes the
  worktree the failed network step took back with the step's own command (the `case_changes` trick) and needs
  `python3` for the `_ping` socket, whose guard must stay **above** `stop_serve`.
- **The merge shares the changes view's *rules* and not its *sentences*.** `changes::basis`/`dirty` are
  `pub(crate)`; the four `no_worktree` messages are the merge's own ("…so there is no branch to merge") because a
  message must describe its own request, which is why `ApiError::no_worktree` takes its message from the caller at
  all. A merge's git failure answers `internal("merge", …)` — a failed *write* logged as "the changes view could
  not read the repository" is a sentence about the wrong operation. The new codes are `bad_target` and
  `bad_strategy` (422), `not_fast_forward`, `target_dirty` and `target_checked_out` (409), and `bad_body` has a
  fourth sentence (`ApiError::bad_merge_body`) for the reason it has a third. The fifth, `unsupported` (422),
  retired with its last field in HS-056 (ADR-0065 D9).
- **`git worktree list --porcelain` prints `prunable <reason>` for a registration whose directory is gone**, and
  such a registration is **not** a checkout: it has no working tree to move, so counting it turns a merge the
  operator can perform into `409 target_checked_out` with advice ("check out another branch in that worktree")
  that cannot be followed. `worktree::Checkout` carries the flag and the merge filters it out; the startup sweep
  still sees every registration. `a_prunable_registration_is_not_a_checkout_and_the_merge_lands_by_the_ref`
  removes a checkout's directory and merges anyway, and dropping the filter turns it red (measured). The parse is
  a separate `parse_checkouts(&str)` so the four block shapes git prints — branch, detached, prunable, the main
  checkout — are testable with no repository.
- **The ancestry test must be asked about the commit the merge will write, not about the branch's ref.** `head`
  comes from `basis`; asking `merge-base --is-ancestor refs/heads/<target> refs/heads/agent/<id>` instead
  re-reads the branch, and a commit landing between the two reads would advance the target to a commit that is no
  longer the branch's tip while answering `200` — reachable when this was written because rule 1's
  `session_running` guard was HS-056's and absent. The guard exists now and does **not** close that window on its
  own: it is one read of one column, and a prompt arriving after it turns the session `running` again. What closes
  it for `delete_session` is comparing the branch's **tip** with the commit the merge measured before the branch is
  dropped (ADR-0065 D6); the read of `head` here is still the one the write uses, and the test, the write and the
  re-measurement still share it.
- **The target's membership is checked twice, on purpose, and once of them is early.** The handler asks
  `merge::listed` straight after the body so one bad `target` is one answer whatever the session's shape; the
  first draft asked it after the row's `base_commit` and so answered `409 no_worktree` for a body that would have
  been `422` on a session with a worktree. `merge()` asks it again as its own precondition.

### Landmines: the two landing strategies (HS-055)

- **Git's own default merge message is `Merge branch 'refs/heads/agent/<id>' into HEAD` in a temporary
  worktree, and the plan to use it went red on the story's first test run.** A detached checkout has no branch
  name to say "into", and the argument this module passes is a fully qualified ref (so a tag called `main`
  cannot be what a merge lands into), so both halves of the sentence are this build's internals. The build
  writes `Merge agent/<id> into <target>` and `Squash agent/<id> into <target>` instead — one shape for both,
  because `--squash` has no default at all. Any future `git` invocation whose *prose* reaches the operator has
  the same trap: run it once and read what it says before deciding it reads well.
- **A commit this build makes must carry `-c user.name`/`-c user.email`, and on this machine that is
  measurable only by reading the commit.** The global config here is `Adam Olsson`, so dropping the two pairs
  leaves every test *green* unless something asserts the author. `snapshot::IDENTITY` is reused (`holosmith
  <holosmith@localhost>`) and both integration tests read `git log -1 --format='%an <%ae>'`; `support::git`
  passing its own `-c` pairs per invocation is why the merge's own commit — made by `git_output` — is the one
  that would have been wrong.
- **`git worktree add` refuses a path that has a *prunable* registration**, and a killed merge leaves exactly
  that shape. So the preparation is `worktree remove --force` first, `remove_dir_all` **measured by
  `path.exists()`** (git does not know a directory it never registered, and its exit status says nothing useful),
  and then `worktree prune` **unconditionally** — prune is a no-op for every registration whose directory is
  still there, which is every live worktree, and it is the only thing that clears the prunable case where the
  directory is already gone. Dropping the whole preparation turns
  `a_stale_temporary_worktree_from_a_killed_run_is_removed_and_the_merge_lands` red; keeping it but skipping
  `prune` turns it green and leaves the rare half unhandled.
- **The teardown belongs *before* the target moves, and the `?` is what makes it easy to get wrong.** `409
  target_dirty` is discovered by running the fast-forward in the operator's checkout — long after the landing
  succeeded — so a `let new = land(...)?; remove(...)` shape leaves the worktree behind on exactly the failure
  the acceptance names. Moving the removal after the move turns **eight** tests red, the conflict ones included
  (a conflicted worktree is dirty and needs `--force`). The cost is a dangling commit when the move fails; its
  id is in the log line, and leaving the worktree for inspection would fail "removed afterwards even on
  failure".
- **A conflict is the *index*'s answer, so `git ls-files --unmerged -z` and not `diff --diff-filter=U`.** The
  `-z` record is `<mode> <sha> <stage>\t<path>\0`, one per stage, so the walk takes what follows the tab and
  dedupes — a path conflicted at three stages appears three times and the operator acts on it once. `ls-files`
  runs no diff driver and no `diff.external`, which is ADR-0062's lesson applied one command further out. A
  non-zero exit **with** no unmerged path is not a conflict: `git merge` also fails for "not something we can
  merge", so that arm is a `500` with git's words, measured apart from the conflict arm in the same function.
- **`git merge --abort` is not run, and the reason has to be written down or the next agent will add it back.**
  The abort *is* the temporary worktree's removal: `merge --abort` restores a working tree that is about to be
  deleted, and for a `squash` there is no `MERGE_HEAD` for it to restore from at all. What "nothing changes"
  means is measured about the **project** repository — the target ref, the checkout's files, `git status
  --porcelain` — because the merge never ran there.
- **A `squash` with nothing to squash makes `git commit` *fail*, and that failure is the no-op.** After a merge
  the target is the branch's history, so `merge --squash` stages nothing and `git commit` refuses with "nothing
  to commit" — prose, not a contract. The answer is measured with `git diff-index --cached --quiet HEAD` (exit
  0 = nothing staged) and answered as a `200` naming the target's own commit, exactly HS-054's second-merge
  shape. Reading the sentence instead is how a `409` for a request that is already satisfied would get written.
- **`merge` and `squash` must not ask the fast-forward question, and the test that proves it is a pair.** The
  same repository shape — a target with a commit the branch lacks — is `409 not_fast_forward` by the default
  strategy and a two-parent `200` by `merge`, in `a_merge_lands_a_branch_the_target_has_already_moved_past`.
  Making the ancestry test run for every strategy turns five tests red, so the decision is measured rather than
  asserted.
- **The lock's wait is observable now, and it was not for `ff-only`.** Two `merge`s of one project's two
  sessions from a `Barrier` both land, and the target and its first parent *are* the two merge commits; without
  the lock both landings are built from the same `old` and the second's move fails. That is a client-visible
  measurement of rule 2, on top of HS-054's mutex-identity test (which stays — it is deterministic and the
  barrier test is not).
- **`ErrorBody` grew one optional key and that is the whole envelope change.** `conflicts` is
  `skip_serializing_if = "Option::is_none"`, so every other error's body is byte-identical, and
  `every_bad_request_moves_no_ref` asserts a non-conflict carries no `conflicts` key — a `409` whose message is
  a fixed sentence (ADR-0008) cannot carry paths any other way. ADR-0052's additive `kept` on the delete's 202
  is the same shape.
- **`case_merge` asserted `422 unsupported` for `strategy:"squash"`, so the gate had to change with the
  feature.** The entry was replaced by `strategy:"rebase"` → `422 bad_strategy`, so the refusal table is still
  walked end to end. Any story that *builds* something a previous `case_*` asserted was refused has this same
  obligation, and it is invisible in `cargo test`.
- **The gate case boots its own **data root** as well as its own projects root**, because `<data>/merge` is what
  it measures. `find "$data/merge" -mindepth 1 | wc -l` and not an inference: the case asserts the directory
  exists first, so a merge that never created one cannot pass by measuring nothing.
- **A squash leaves the branch a non-ancestor of the target, so HS-043's keep-rule keeps it — and that is a fact
  HS-056 inherits, not a bug here.** `Worktree::merged_into` is `git merge-base --is-ancestor agent/<id>
  <target>`, and a squash's commit has the *old target* as its only parent, so the branch's tip is not in the
  target's history. `retained` therefore answers `branch: Some(agent/<id>)`. That is right — the branch holds
  the session's commits, and a squash deliberately keeps them out — but it makes rule 5's "the branch is deleted
  because it is merged" a decision for HS-056 rather than a git question, which ADR-0064 D11 records.
  `a_squash_keeps_the_branch_a_merge_does_not` asks `retained` after each strategy so the assumption is a red
  test rather than a surprise.
- **`config.worktreePruneExpire` does not stop `git worktree prune` here (measured on 2.43), but the *needle* in
  a test about a prunable registration must not be the word alone.** A fresh worktree whose directory is removed
  becomes `prunable gitdir file points to non-existent location` and a plain `git worktree prune` clears it, so
  `clear`'s unconditional prune is what lets `worktree add` take the path back. The trap is the assertion: the
  first version of the new test asserted `listing.contains("prunable")` while its fixture was labelled
  `strategy-prunable`, so the *temporary directory's own path* satisfied it — vacuous, and it hid the fact that
  the test's second half was failing for a real reason. Assert `"prunable "` with git's reason, keep the word out
  of the fixture's label, and note that HS-054's `a_prunable_registration_is_not_a_checkout_and_the_merge_lands`
  had the identical defect (fixture `merge-prunable`) and is fixed here.
- **The refusal order is strategy-dependent and that is deliberate.** `ff-only` asks the ancestry question before
  the checkout one (HS-054's order: "check out another branch in that worktree" is advice that leads straight to
  a second refusal), while `merge`/`squash` ask the checkout question first (a `409 target_checked_out` should
  cost no temporary worktree). One shared function would have silently swapped HS-054's order for its own path;
  `the_fast_forward_refusal_comes_before_the_checkout_one_and_the_landings_do_not` pins both halves.
- **A `strategy` that is not a string is `bad_body`, not `bad_strategy`** (HS-054's string comparison answered
  the latter). It is the body's *shape* rather than a value outside the vocabulary — the create's split for a
  numeric `model` — and the wire table asserts it rather than leaving the reclassification to a reader of
  ADR-0008.

### Landmines: the merge's guards and the delete after a merge (HS-056)

- **The keep-rule's override is only observable where the backend owns a repository, so the gate is its proof and
  the crate tests are not.** `tests/merge.rs`'s `Fixture` uses the stub backend, whose `retained` answers
  `Retained::nothing()` and whose `delete` removes nothing — so `delete_session` there can assert the row goes and
  the answer keeps three keys, and it cannot assert anything about a branch. `case_merge_guards` is where the
  branch/worktree half is measured, and it is measured by *deleting a squashed session and reading
  `git branch --list 'agent/*'`*: with the override, empty; without it, `  agent/<id>` — the mutation was run and
  the case fails on exactly that line. A story that adds a `delete_session` assertion at the crate level with a
  stub fixture is asserting the stub's shape, not the rule.
- **`changes::dirty` in a directory that is not there is a `500`, so the guard asks `changes::basis` first.** The
  changes view's four ordered refusals are what turn an `rm -rf`'d worktree into `409 no_worktree`; asking `dirty`
  alone answers `internal`. `a_session_with_no_worktree_is_refused_before_the_dirty_check` is the test, and
  deleting the `basis` line turns it red while every other merge test stays green — which is why the line needs a
  test of its own rather than a comment.
- **The 60-second pair has no client in this build, and a blanket `fetch` timeout would be a bug rather than a
  feature.** Rule 6's pair is the *hub's* (HS-069's slice already carries "Default forwarding timeout is 5 s; merge
  and publish use 60 s") and the buttons are HS-058's. `DELETE /api/sessions/{id}` legitimately waits out the
  abort ladder's `abort_grace` — ten seconds by default — before it takes a snapshot, so a 5 s default on
  the client's own `fetch` would cut the delete's own bounded wait and answer a client that its request failed
  when the removal is running — and the delete's call is its own `fetch` (`main.ts`'s `deleteSection`), not the
  composer's `post` helper, so "one shared timeout" would have to be added twice to break two things. ADR-0065 D8 records it; this bullet exists so the next agent does not
  "complete" the acceptance bullet by adding a timeout to the shared helper.
- **A gate that needs a `running` row writes one column, and that is a fixture rather than a shortcut in the
  code.** The gate's Docker boots are a `_ping` shim, so a session's network step fails and no agent ever starts —
  there is no way to make a real turn. `case_merge_guards` sets `sessions.status = 'running'` with `sqlite3` from
  `python3` (the file is in WAL, so a committed write from another process is what the server's next read sees) and
  then sets it back to the `error` its failed provision left, which is also the *negative* half: the row an
  `!= Idle` guard would refuse. `tests/merge.rs` drives the same row through the server's own `Transitioner`, so
  the state machine is honoured where it can be.
- **Building a field a previous `case_*` asserted was refused changes the gate, and it is invisible in
  `cargo test`.** `case_merge`'s refusal table listed `{"delete_session":true}` → `422 unsupported`; it now merges
  (and deletes), so the entry left the table, and the two entries that remain are the two `bad_target`s and
  `bad_strategy`. `delete_session`'s own arm is a case of its own, because it needs a repository. ADR-0064 recorded
  the same obligation for `strategy:"squash"`, and it will happen again.
- **`unsupported` is gone, and the code is edited where it is named rather than left behind.** It existed for two
  requests ADR-0063 could not honour; HS-055 built one and HS-056 the other, so the constant, the constructor and
  the wire-table row went with them — `docs/build.md`'s `case_merge` paragraph included, because a gate description
  that still lists a code nothing sends is a document lying about the artifact.
- **A rule that "the request knows better" must still compare a commit, and the first draft did not.** The plan
  was for `delete_session` to force `Retained { branch: None }` in the handler, because the request that merges is
  better placed than `merge-base --is-ancestor` to say "this landed" (a `squash`'s commit is nobody's ancestor).
  The review found the window that shape opens: `merge()` writes the commit `basis` read, so an agent's commit
  between that read and the delete is work nothing landed — and a `match` in the handler cannot see the branch.
  The fact therefore travels into `Worktree::retained` as `landed: Option<&str>` and the rule is a **tip
  comparison** (`landed == Some(tip)`), with the ordinary ancestry question answering everything else. Both halves
  are one test (`the_commit_a_merge_landed_is_not_a_branch_to_keep_and_one_that_moved_is`), because "keep
  everything" and "delete everything" each pass one of them alone.
- **An assertion on a sentence's *next step* must not use a word the first half already contains.**
  `assert!(message.contains("commit"))` over "the session has **uncommitted** work, so nothing was merged; ask the
  agent to commit it" passes with the whole offer deleted — the review's P1, in both the crate test and the gate.
  The needle is `ask the agent to commit`, and the way to find this class is to delete the half the comment above
  the assertion says it measures and watch: it stayed green.
- **`sessions::deletion` returns `DeleteAnswer`, and the merge ignores it on purpose.** The answer is the 202's body
  and the merge's is rule 6's three keys; making the merge's answer additive would make its shape depend on the
  request (ADR-0065 D7). What the merge does with a *refusal* is log it and keep the `200`, because a `409` after
  the target moved would tell the operator the work did not land. The test for that arm is the gate's: the
  session's `sessions` directory made read-only makes `snapshot::take` fail, and the merge still answers `200`, the
  row still exists, and its status is not `deleting`.

### Landmines: the publish, and a push into somebody else's repository (HS-057)

- **`git push --porcelain` puts the rejection on *stdout*, in a documented machine format, and the reason word
  changes with what the local repository happens to know.** A non-fast-forward whose remote tip is not in the
  local object store is `!	<from>:<to>	[rejected] (fetch first)`; the same fact with the object present is
  `(non-fast-forward)`. So the classification is the **`!` flag**, and the reason is kept verbatim for the log
  only — a build that matched on `non-fast-forward` would report a `500` for the ordinary case. The prose on
  stderr (`error: failed to push some refs …`, the `hint:` block) is not read at all.
- **A repository's `push.followTags = true` makes `git push <remote> <refspec>` carry annotated tags in
  addition** (measured: `refs/tags/v1` landed on the remote without the flag, and not with `--no-follow-tags`).
  Same class as ADR-0062's `diff.external`, and the same fix: pin the flag, then make a repository config set
  the thing and watch the test go red. `push.gpgSign` is the same class and *cannot* be measured here (no key,
  no signing program) — recorded in ADR-0066 rather than pinned blindly.
- **`-c core.hooksPath=/dev/null` on a push is load-bearing for the same reason it is on a merge**, and the
  measurement is identical: a real `.git/hooks/pre-push` that `touch`es a marker and exits 1 fired without the
  flag and never with it. Hooks live in the **common** git directory, so the project's hooks are reachable from a
  push that never enters the worktree — `-C <repo>` is not a defence.
- **A server has no terminal, so a credential prompt is a wedged request, not a slow one.** `GIT_TERMINAL_PROMPT=0`
  on the child turns it into git's own error. It does not remove the host's credentials: helpers,
  `~/.git-credentials` and an `ssh-agent` are still consulted, which is what "the host's own git" means.
- **A bare repository under the test's own `TempDir`/`$WORK` is the only honest way to measure a push without a
  network — and it is also the measurement of "the host did it".** The remote is on a path no container's mount
  vector names, so a `200` plus `git rev-parse` in the bare repository cannot be a container's work; and
  `git remote get-url <name>` exits **2** for a remote the repository does not have (0 with the URL), which is
  what turns "configured but absent" into a `422` instead of a `500` carrying `fatal: 'origin' does not appear to
  be a git repository`.
- **A gate case whose second refusal is unreachable under the default config measures nothing.** With
  `[merge] targets = ["main","master"]`, a destination called `main` is *protected* first, so
  `target_not_pushable` could never be reached — `case_publish` sets `targets = ["trunk"]` and asserts both orders,
  and `tests/publish.rs` asserts the precedence separately (`main` on a machine whose target is `main` is
  `protected_branch`).
- **`config::is_ref_name` was not git's refname alphabet, and `git push` is where that shows.** It checked the
  whole-name rules and the forbidden characters but not git's **component** rules, so `a.lock`, `.hidden` and
  `a/.b` passed it and reached `git push`, which refuses each with `fatal: invalid refspec` **before** contacting
  a remote (measured) — a `500` for the client's mistake, with a sentence about the remote. `git check-ref-format
  refs/heads/<name>` is the oracle: it also refuses `a//b` (an empty component) and allows `a./b`, `x.locked` and
  `a/b.`'s *component* (only the whole ref may not end with a dot, which the predicate already checked). The
  predicate now refuses an empty component, one beginning with `.`, and one ending in `.lock`; because it is the
  same predicate `merge.targets` is validated with, that config got stricter in the same commit.
- **An inherited environment is not measurable by `Command::get_envs`.** That reports only what the command sets
  *explicitly*, so `.env_clear()` would keep the unit test green while destroying "the host's own git
  credentials". The measurement is a boot with git's own `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_0`/
  `GIT_CONFIG_VALUE_0` naming a `remote.origin.pushurl` for a second bare repository: a push that inherited the
  environment lands there, and one that did not lands at the URL the repository names. `case_publish` asserts
  both halves (the ref in the pushurl repository, the original's refs unchanged).
- **The disabled endpoint is the process's config, so its gate arm is a second boot**, not a request against the
  first: `[publish] remotes = []` in a rewritten config, `start_serve` again over the same data root, and the
  `403` asserted for the existing row *and* for an id the table does not hold (the claim that it is answered
  before the row).

### Landmines: the Changes sheet (HS-058)

- **A gate case cannot see a DOM, so a UI story's case measures *shipping* and its rules are `npm test`'s.** RESUME
  §3's "no gate may need a browser" is not a preference: `case_changes_screen` asserts the served page is
  `web/dist/index.html` byte for byte, that the **served bundle** is `web/dist/assets/…` byte for byte, and that the
  bundle carries the sheet's nineteen `id="…"` needles and the three ADR sentences. The `cmp` comes first on purpose — a
  grep alone would pass over a stale `dist/` that happened to carry the strings. The mutation is deleting a rule,
  rebuilding, and watching the grep go red; the weak half (a string in a bundle is not a rendered element) is why
  the sentence is written down in the ADR instead of being implied.
- **`element.id = "changes-merge"` survives minification; a *class* name is not what to grep for.** Vite's minifier
  keeps string literals, so ids and sentences are stable needles — but a needle chosen from a `className` would also
  match the CSS bundle's own text, which is a different file, and one needle (`changes`) matches half the others.
  The first draft of `case_changes_screen` grepped bare ids, so `changes-conflict` still matched
  `changes-conflict-text` and `changes-publish` matched `changes-publish-start`: deleting the container element
  would have left the case green. The needles are now `id="changes-conflict"` — the assignment's own quotes, which
  no sibling's id contains — and the case counts every needle that found something, so a needle that found nothing
  cannot read as one that did.
- **The client's disabled reason must be the route's order, and "only `running` refuses" is a client fact too.**
  `mergeBlocked` asks the row, then the target, then `dirty` — `http::merge`'s order — and it must not be written as
  `state !== "idle"`, which is the same shape HS-056's review caught server-side: four of the five states still
  merge, and the `error` row a failed provision leaves is the one an operator most wants to merge. The
  `the merge is blocked by running, and by nothing else a row can be` test is what goes red.
- **A re-render during a request re-enables its own button unless something says otherwise.** The sheet re-renders on
  its own 202 (`showState` → `update` → `render`), so without a `busy` flag the merge's button was enabled again
  mid-merge and a second tap was a second merge. Every action sets it, and `render()` is the one place every
  element's state is decided — the same "two writers of one value" lesson as HS-020's, in a DOM instead of a row.
- **`check-static.sh`'s empty-root aarch64 `serve` waits 200 × 50 ms and can lose that race under load.** This
  session's baseline run failed there ("serve printed no control_listen line:" with *empty* stderr) while a `cargo
  check` and a few large greps were running in the same tree; the same binary booted in well under a second on a
  bare re-run, and the next full gate was green. So: **nothing else runs while `verify.sh` runs**, not even a read
  that spawns a compiler, and a failure there is re-measured alone before it is believed.
- **The target walk no longer stops at the first hit.** `changes::existing_targets` asks git about every configured
  name (two by default) because the whole list is the picker's options; `first_target` is gone, and a repository
  with `main` and `master` now costs two `rev-parse --verify` calls instead of one. `choices.targets` is filtered
  because an option `merge` refuses is a button that cannot work; `choices.remotes` is **not**, because whether the
  repository has the remote is the publish route's question and its own `422 bad_remote` sentence (ADR-0067 D4).

### Landmines: the disk floor (HS-062)

- **A machine-wide check on a request path multiplies by the test suite.** This sandbox's `/tmp` has **3 GB**
  free and `min_free_gb` defaults to **20**, so a floor asked unconditionally would turn every stub create in
  `cargo test` into a 507 — 26 test files create sessions. The fix is not `min_free_gb = 0` everywhere (that
  hides the code path from every test that exercises it): the *predicate* rides on the preflight's own
  (`disk::guarded`: Docker and not a `hub`), read from `runtime.backend`, which is what `serve` built `Backends`
  from. Any later machine-wide refusal meets the same arithmetic — measure the blast radius in `/tmp` before
  wiring it in.
- **An async test that writes a shim and forks it needs `tokio::sync::Mutex`, not a `std` one.**
  `tests/preflight.rs`'s `one_at_a_time` lock exists because `exec` of a file another thread still holds open is
  `ETXTBSY`; in an `async` test the same lock is held across `.await`s, and `clippy::await_holding_lock` (denied
  by `-D warnings`) refuses a `std` guard there. `tests/disk.rs` uses `tokio::sync::Mutex::const_new(())`
  and `.lock().await`, so the guard still spans the whole body.
- **A `case_*` that stops the shared boot must put it back.** `case_disk_floor` boots its own server and ends
  with `stop_serve` + `start_serve` (`case_delete_session`'s idiom) even though the cases that follow it happen
  to boot their own: `stop_serve` fails hard on an empty `serve_pid`, so leaving it empty makes the *next*
  unguarded `stop_serve` a phase failure that only shows up when someone reorders the list. The review caught
  this one, not the gate.
- **The disk floor's sentence is one sentence for two requests, and that is why it says "this machine starts
  nothing".** A create makes another session and a resume brings an existing one back, so "no new session" was a
  create's sentence being printed for a resume — the exact shape ADR-0008's "a message must describe its own
  request" forbids, and the reason `cap_reached` has two sentences. When two requests share a fix (item 14 gives
  a create and a resume the same one), they share the sentence; when they do not, they must not.
- **`ErrorBody.message` is a `Cow<'static, str>` since HS-062**, so a refusal can carry a measurement. Every
  earlier constructor still passes a literal and serialises byte-identically, and `internal` still has a fixed
  sentence (no rusqlite text on the wire). A new code that wants numbers passes `Cow::Owned`, and a `tracing`
  field that carried `message` now needs `%error.body.message` — `Cow` is not a `tracing::Value`.

### Landmines: the per-session disk cap (HS-063)

- **A periodic policy that has to end a *turn* cannot use `Ladder::climb`.** `climb` is the two rungs *plus the
  way back* (ADR-0020): a forced stop is resumed at once, because a killed container is not a session the operator
  asked to keep stopped. A disk cap is the opposite — the session must stay `stopped` until somebody cleans the
  worktree up — so the third caller uses `end_turn`, and when rung 1 works (the runner wrote `idle` with
  `abort_completed`) the idle arm's own table row finishes the job with *its* reason. Two endings, one policy: a
  test that asserted only the forced arm would never notice the asked arm leaving `abort_completed` on a
  quota-stopped row.
- **A boot-time pass sees `starting`, not `idle`.** The reconcile moves every live row and re-provisions it
  (HS-044), so "sweep at spawn" measures nothing at exactly the moment a restart's stale session is the thing to
  catch. `Quota::run` waits for no `starting` row (bounded at 60 s) before its first pass. That is also what makes
  the gate cheap: `case_disk_quota` restarts the server and reads the stop a couple of seconds later instead of
  waiting the spec's five minutes — a case that waits the interval is not a gate.
- **`session_disk_soft_gb = 0` must never reach the comparison.** `over(size, 0)` answers `true` for every
  worktree, so "off" is `AppState::quota()`'s `None` rather than a number the sweep hands to `over`; a test asserts
  the `None` and another pins the comparison's own answer, so the trap is documented instead of discovered.
- **A `du` stand-in is the only way a gate can measure a size, and its arguments are the witness.** The case's shim
  logs `-sk -- <path>` and answers a fixed size: the size is the stand-in's and the *path* is the story ("each live
  session's worktree"). Assert the path, not the size — a case that only lowered the cap would pass whether or not
  the watchdog measured the right directory.
- **`--one-file-system` is the flag this policy must not grow**, and two tests see two different halves of that.
  It is GNU-only, while `du` here must be a POSIX utility, and it would leave a nested mount out of a worktree's
  size. The **argument vector** is pinned by a shim that records what it was asked (`-sk -- <path>`, nothing
  else), which is the half that would catch the flag; the acceptance's "the cap includes `target/` and
  `node_modules`" is the *inclusion* half, and it needs the machine's own `du` over a tree that has both
  directories in it — a one-filesystem tree, which is why that test cannot be the one that catches the flag.
- **An SSE frame's kind is its `event:` line, not a payload key.** `case_disk_quota`'s first draft asserted
  `"type":"error"` against the stream — that field belongs to a history *page*, whose events are objects, while a
  stream frame is `id`/`event`/`data`. The case failed on its own wrong needle, not on the policy; the page keeps
  the `type` assertion and the stream asserts `event: error`.

### Landmines: stopped-session expiry (HS-064)

- **The story's destructive decision is a table row, not a comparison in the job.** `Trigger::Expire` admits
  `stopped` only, so a session the operator resumed between the pass's read and the flow's write is refused by
  `Transitioner::apply` — ADR-0016's one authority kept. The pass *also* re-reads the row, and that re-read is
  cosmetic (it keeps the log line honest); the safety is the table. Two guards that agree make a test that
  cannot tell them apart: `tests/expiry.rs`'s "a row that left `stopped` is not expired" proves the *outcome*,
  and `tests/state_machine.rs`'s `the_expirys_row_admits_stopped_and_nothing_else` is what isolates the row —
  change `FromStates::OneOf(&[Status::Stopped])` to `Any` and that test goes red (as do the table's own
  row-by-row test and its `lookup(Deleting, …)` spot check, which is the point: the table is one fact with
  three witnesses).
- **`set_status` is the only way to backdate a stop, and it needs no second statement.** `set_status` writes
  `stopped_at = now` when the new status is `stopped`, so passing a `now` of eight days ago *is* the backdate.
  `Database` has no public `connection()`, so a test that reached for `UPDATE sessions SET stopped_at = …`
  cannot run at all — which is the good result: item 15's one writer is the only door.
- **A `stopped` session is not reconciled, so the expiry's first pass needs no `SETTLE`.** The startup
  reconcile moves every **live** row to `starting` and re-provisions it; `stopped`, `error` and `deleting` are
  untouched. That is why `expiry::run` sweeps immediately where `quota::run` waits — and why the gate and the
  demo observe a daily policy in seconds (backdate the row, restart, read the disappearance). Copying the
  quota's settle into a later policy over non-live rows would be a 60-second delay for nothing.
- **The pass's status re-read is isolated in-crate, because the interleaving cannot be built outside it.**
  `Expiry::expire` is private, and an integration test that resumes a row before calling `sweep` proves only the
  *statement's* predicate (a resumed row is not a candidate). The guard "do not hand the flow a row that left
  `stopped`" is `src/expiry.rs`'s own `#[tokio::test]`: it reads a stale candidate, resumes it through
  `Transitioner`, then calls `expire` with the stale row and asserts the stub recorded **no `Snapshot`** —
  which is what makes deleting the re-read go red, where the row's survival alone would not (the table refuses
  the move either way). A guard whose only effect is "we did not do expensive work on a row we would refuse"
  needs an assertion about the work, not about the row.
- **An in-crate test cannot call `set_status`.** `no_status_write_outside_the_one_function` walks `src/**` and
  allows the raw write only in `sessions/repository.rs` and `sessions/state.rs`; integration tests are exempt
  because the walk does not read `tests/`, but a `#[cfg(test)]` module inside a policy is not. So a test beside a
  policy drives the table (`Transitioner::apply`) to reach the state it needs — which is also the honest route,
  and it costs the backdating an integration test can do with `set_status`'s `now` parameter. This one was found
  by the gate, after the review, in a fix the review asked for.
- **The removal is the delete flow's own task, so "the row is gone" is a fact that arrives after the pass.**
  `complete_deletion` spawns `Deleter::remove` and answers; a test that asserted the row's absence straight
  after `Expiry::sweep` is a race (it failed on the first run of `tests/expiry.rs`). Wait for it
  (`wait_gone`), and note the same is true of the *order* two removals happen in: the repository's
  `ORDER BY stopped_at` is asserted where that statement lives, never through two concurrent tasks.
- **"An `error` row is never auto-deleted" has two independent guarantees, and only one is testable in-process.**
  The candidate query's `status = 'stopped'` predicate and `stopped_at`'s `NULL` in every other state agree on
  every row this build can write, so a test cannot isolate either. `case_stopped_expiry` writes `status='error'`
  **with** a stale `stopped_at` — a row no writer produces — which is the only construction where the predicate
  is load-bearing. A story that reasons about a policy's refuse-list should look for that shape.
- **The days-remaining sentence is a `transcript.ts` rule with its clock passed in, and two places draw it.**
  `expiryText(stoppedAt, ttlDays, now)` is pure; `main.ts` reads `Date.now()`. The `session.status` frame
  carries no `stopped_at`, so the patch path uses the frame's arrival as the stop instant — right to the second,
  because the transition that emitted it just wrote the clock. The machine's window reaches that path through
  `<ul id="session-list" data-stopped-ttl-days>`, because `main.ts` keeps no state beyond the DOM and a create's
  answer carries a row without the setting.
- **The sentence must not promise a delete.** The job snapshots first and skips a row whose snapshot fails, so a
  stopped session can outlive its window with its work intact: "expires in N days" is what the answer can
  stand behind, and a test asserts the word "delete" never appears.
- **`format!` with no arguments is a `String` allocated for nothing.** The repository's new filter is a `const`
  where `idle_before_filter` is a `fn`: the latter interpolates `QUIETEST_FIRST`, the former has nothing to
  interpolate. `clippy::useless_format` would have caught it, but the shape is worth choosing deliberately.
- **A `pub(crate)` function in a private module is not reachable, even from the crate.** `http::sessions` is
  private, so `complete_deletion` needed one `pub(crate) use` line in `http/mod.rs`; making the module `pub`
  would have opened the whole handler file to the crate to expose one name.

### Landmines: the push, the pair and the payload (HS-066)

- **`openssl enc` cannot do AEAD on this host.** `enc -aes-128-gcm` answers `AEAD ciphers not supported`
  (OpenSSL 3.0.13), with `-iv`, `-K` and `-tag` all spelled correctly. That one measurement is why RFC 8291's
  record is written in Rust; a story that needs a second AEAD mode should re-measure before assuming the CLI is
  a route.
- **`openssl kdf … HKDF` answers the wrong bytes for its published vector.** With `hexkey`/`hexsalt`/`hexinfo`
  set to RFC 5869 case 1's inputs it does not produce that case's OKM — the CLI's "hex" parameters are hex
  *strings*, not the bytes they spell. A key schedule cannot be a child process this way; that is the second
  half of why `push::crypto` exists.
- **`SSL_CERT_FILE` is the trust-store seam, and `-CAfile` would have made `case_push` impossible.** A child
  inherits the process's environment, so a gate can export `SSL_CERT_FILE` around the boot and `s_client` trusts
  its own CA; a hard-coded `-CAfile` would have meant the gate could not stand a private CA in front of the
  sender at all. The key proxy passes `-CAfile` on purpose (it must reach a provider, not this machine's CAs);
  the sender deliberately does not.
- **`-verify_hostname` matches a `DNS:` SAN, not a CN.** A certificate made with `-subj /CN=localhost` and no
  `-extfile subjectAltName=DNS:localhost` fails the handshake, and the failure reads like "the sender cannot
  verify anything".
- **`s_client` without `-quiet` prints session and certificate chatter on stdout**, before the response, so a
  status parser that reads line one gets a certificate's subject. `-quiet` is load-bearing, and `status_of`
  scans for the first `HTTP/` line anyway.
- **Closing `s_client`'s stdin makes it send its close-notify before the response arrives.** The key proxy
  measured this (ADR-0048) and it did not change here: the request is written, the handle is held, and
  `Connection: close` is what ends the exchange. A "write, close, read" shape looks right and intermittently
  delivers nothing.
- **RFC 8291's `Content-Length: 145` is an erratum.** §5's body is 144 octets — 86 of header and 58 of record —
  and the golden test pins 144. Nothing else in the example is wrong: the body and all five intermediates
  reproduce byte for byte.
- **The header's `0x41` is an `idlen`, not a key length to be computed.** `salt(16) ‖ rs(4) ‖ idlen ‖ keyid`,
  where `idlen` is 65 — `0x41` — and the key id is the application server's 65-octet point. A reader that
  assumes a fixed 65 gets away with it until a receiver sends something else.
- **`session.status`'s payload stayed four keys.** `{from, to, step, reason}` is pinned and gains nothing for
  this story: the pusher reads `to` and `reason` out of it as JSON, and a fifth key ("kind", "elapsed") would
  have been the second vocabulary this build keeps refusing to add. The duration comes from the `turn.start`
  frame's *arrival*, which is why a restart mid-turn cannot invent one.
- **A push endpoint is a capability URL: the log names its host and nothing else.** Anybody holding the URL can
  send to that browser, so `push`'s every line prints `host = %audience` (the origin) and never the endpoint,
  and the recorder-based tests assert the endpoint rather than logging it.
- **`crypto.verify` takes a bare JWK only with `format: 'jwk'`.** The gate's receiver passed the application
  server key as `{ key: jwk, dsaEncoding: 'ieee-p1363' }`, where `jwk` is a JWK object; node reads `key` as a
  `KeyObject` unless `format: 'jwk'` says otherwise, throws, and the `catch` around it reported *"the machine's
  signature does not verify"* for a signature that did. `crypto.createPublicKey({ key: jwk, format: 'jwk' })` once,
  then pass that. The demo's receiver had a second, independent bug in the same place (it called `setAuthTag`
  *after* `final()`), which is why the two must not share code: each found the other's mistake only because the
  sender was measured by code that did not.
- **`fetch_stream` closes its socket, so it cannot hold a stream open.** The helper reads the header block and
  returns, which ends the SSE handler and drops its `StreamGuard` — so a case that opens a stream with it and *then*
  prompts has no stream open at all, and an assertion that "nothing was pushed" would fail for the right reason and
  read as the wrong one. Hold the descriptor across the turn (`exec {fd}<>"/dev/tcp/…"`, read the header, prompt,
  only then close) — which is also what a locked phone looks like from the server's side.
- **A gate case is code that has never run until it does.** `case_push` was written, reviewed and green-typed by
  three bugs in a row — a broken DER walker, a receiver that could not verify, a socket that was already closed —
  and each surfaced only in a real `scripts/verify.sh`, one run at a time. Budget one gate run per non-trivial new
  case, and read its first failure as the case's bug rather than the feature's until proven otherwise.
- **A test that writes `<data>/secrets/vapid.toml` must own its data root.** `Config::default()`'s data root is
  the machine's (`/var/lib/holosmith`), which this sandbox lets an unprivileged user write: two tests sharing it
  passed until one of them removed the pair for its own negative case, and then a third failed. Every test in
  `tests/push.rs` builds its config from `TempDir::join`, the same rule every database in the suite follows.

### Landmines: the installable app (HS-067)

- **Chromium's install criteria do not need a `fetch` handler, and `Page.getInstallabilityErrors` is how that is
  measured.** The plan's "no `fetch` handler" (ADR-0072 D4) looked like it might cost an install prompt, because
  older guidance says Chrome requires a registered worker *with* a fetch handler. Measured against the shipped
  artifact in Chromium **141.0.7390.37** over CDP: `Page.getInstallabilityErrors` answers exactly
  `[{"errorId":"in-incognito"}]` — the automation profile, not the app — and `Page.getAppManifest` reports no
  errors. So a cache-less worker is installable, and the demo asserts that list rather than a rendered prompt.
- **Playwright's headless *shell* reports `Notification.permission` as `denied` and cannot be granted it.** The
  settings screen's Subscribe button is therefore unreachable in the default `chromium.launch()`: use
  `channel: "chromium"` (the full build), where `context.grantPermissions(["notifications"], {origin})` answers
  `granted`. The shell says `denied` for every origin, which reads exactly like an app bug.
- **A service worker needs `navigator.serviceWorker.register`, and no gate can see it missing.** The first draft of
  this story shipped the manifest, the served `/sw.js` compared byte for byte, and no registration at all: a push
  can only arrive at a registered worker, and the gate's nearest witness is a grep for the call in the served
  bundle (`case_pwa` now has one). The demo is what actually noticed.
- **The stand-in subscription has to be `https://`.** The route refuses anything else with `422 bad_endpoint`, so a
  demo that points its synthetic endpoint at its own origin gets a 422 that looks like a key problem.
- **A `page.evaluate` patch of `ServiceWorkerRegistration.prototype` does not survive a `goto`.** A navigation is a
  new realm, so the synthetic `pushManager` has to be installed *after* every load — and the screen's first
  `gather()` then reads the real API, which is the honest order anyway (the state must first be "not subscribed").
- **`waitForFunction` on `textContent.includes("subscribed on this machine")` resolves on "not subscribed…".** The
  settings screen's two sentences are substrings of each other; every wait on them compares the whole string.

### Landmines: the host registry and its probe (HS-068)

- **The probe's answer fits a pipe, so pipes are right here.** `push::send` writes the child's streams to files
  because it must never *read* a pipe whose writer is still running; a health payload is a kilobyte, far inside a
  64 KiB pipe buffer, so the child cannot block writing it while the parent waits. `hosts::probe` therefore spawns
  `s_client` with piped stdio, holds the stdin handle (closing it makes `s_client` send close-notify first), waits
  with `try_wait` against `PROBE_TIMEOUT`, and only then calls `wait_with_output` — by which time the child has exited
  and both pipes are at EOF. Two shapes, two reasons; do not "unify" them.
- **A test stand-in must serve one connection and stop.** The first draft of `hosts::probe`'s unit test looped
  `for stream in listener.incoming()` and then called `JoinHandle::join()`, which waits for the *listener* to end: the
  whole `cargo test --lib` run hung for ten minutes with one test "running" and no error. One `accept()`, one answer,
  and the thread returns.
- **The API's host rule refuses loopback, so the probe's success path is measurable only through the seed.** No
  `tailscaled`, `ip addr add 100.64.0.1/32 dev lo` answers `Operation not permitted`, and no `.ts.net` name resolves
  here — so `case_hosts` seeds `http://127.0.0.1:<port>` and `https://localhost:<port>` rows (the seed is not
  filtered by the predicate, ADR-0073 D6) and stands the listeners in. **The seed needs the port before the boot**,
  which is why `start_serve` grew its optional third argument: a `--port 0` boot's port is exactly what a row naming
  this machine cannot know in advance.
- **`SSL_CERT_FILE` needs no code at all.** Unlike `push::send::SslClient`, which reads the variable into a field, the
  probe's `s_client` child simply inherits the process's environment: exporting it around `start_serve` is the whole
  mechanism, and it works only because no `-CAfile` is passed (ADR-0071). The certificate still needs a **`DNS:`
  SAN** — `-verify_hostname` matches the SAN and never a CN — so the stand-in's URL names `localhost`, not
  `127.0.0.1`.
- **A merge at startup runs before the listener binds, so a local row must not be probed.** Asking the process to
  reach its own origin at that moment is a connect into a bound-but-unaccepted listener (the kernel completes the
  handshake and the read then blocks to the deadline), so the row would be born `unreachable` on a healthy machine.
  The local shortcut is the fix, and the gate asserts the local row is `reachable` with this build's version.
- **`tests/db_migrations.rs` hard-codes the applied set in three places.** A new migration moves all three vectors
  (the first start, the second start, and the two-process race) — `SCHEMA_VERSION` alone is not enough, and the
  failure reads as "the second start rewrote the applied set", which is a false alarm about the wrong thing.
- **The `hosts` SQL guard is a word match.** `insert into hosts` is reported as `into hosts`, and a *documentation*
  sentence containing "from hosts" in another file would trip it — the guard walks `src/**` for text, not for SQL.
- **The gate has no `jq`, so one row's field is `python3`'s.** `case_hosts` defines `hosts_row_field NAME FIELD
  FILE` inside the case; a `sed` over a five-row list would assert the wrong row the first time the order changed.
  The order is `name ASC`, and a row's JSON fields are the struct's order, so a needle can stop before the varying
  `last_checked_at`.

### Landmines: the hub's forwarding (HS-069)

- **`HTTP/1.0` is not the free lunch it looks like — measured.** Asking a peer for a 1.0 answer would make its server
  close-delimit the body, which is what a relay wants; hyper's server will not close-delimit an **endless** one, so it
  sends **no head at all**: `curl -0` against `GET /api/sessions/<id>/events` printed zero bytes in four seconds while
  the same request over 1.1 answered at once. That is why `src/forward.rs` carries a chunked decoder. If it ever
  cannot relay a real peer's stream event for event, the fallback is hyper's `client` feature plus `want` (ADR-0074
  D2), and that trigger is written down so nobody re-litigates it.
- **A stand-in peer on `spawn_blocking` hangs the whole test binary.** `spawn_blocking` tasks cannot be aborted, so a
  `for stream in listener.incoming()` loop keeps the runtime's shutdown waiting forever — the mirror of the HS-068
  landmine (a *stand-in* loop that never ends). `tests/forward.rs`'s `Peer` is a **tokio** task ended by `abort`. And
  a peer that is supposed to say nothing must **hold** its socket: closing it is an answer, and the `504` arm is about
  a socket that is open and silent.
- **A forwarded `Host` and `Origin` must be the peer's *exact* `control_origin`.** The guard compares `Origin` as text
  against the config's own spelling while `hosts::is_local` folds ports, so a row registered as
  `http://localhost:<port>` against a peer whose origin is `http://127.0.0.1:<port>` passes the probe and answers
  `403 bad_origin` on every mutating call. `case_forward` measures exactly that, and it doubles as "a peer's own
  refusal survives the trip". Register machines by the origin they report; it is a precondition, not a check.
- **A stand-in's `Content-Length` has to be computed, not eyeballed.** A reply promising 39 octets and sending 38 makes
  the hub's `Framing::Length` stream error and hyper truncate the answer — which reads in a test as "the relay lost the
  tail" and is really the fixture's arithmetic. Count the bytes (or build the head with `format!`).
- **`Route::strip` no longer decides which host it is, so the slashless redirect moved *after* the plan.** With the
  old order, `/api/health` on the preview listener became a `308` to `/api/health/` (it is `/<host>/<id>` with no
  trailing slash) instead of the `404` `case_preview` asserts. Order in `handle`: split → an upgrade's `Origin` →
  the plan → the redirect → serve.
- **Two boots break four assumptions of `start_serve`.** Its globals (`serve_pid`, `port`, `control_origin`,
  `preview_port`) are one machine's, and its `--admin-dir` comment says "only one serve is up at a time". A case with
  two machines needs `start_peer`/`stop_peer` with their own admin directory and `peer_*` globals, `forward_fetch
  <port>` (because `fetch` always talks to `$port`), a `peer_pid` in the trap, and a `wait` after a `kill -9` or bash
  prints its own "Killed" line into the case's captured output.
- **The gate's `--no-build` run uses whatever is in `dist/`.** Two iterations of `case_forward` failed as bugs of the
  case that were really a stale artifact with no `preview` column. Rebuild after any `src/` change before believing a
  gate line.
- **A container answers a second or two after `docker run` returns, and the *host* can reach it.** Ask it directly
  first (V7's measurement, and a readiness wait) before reading the forward's answer, or a `502 preview_unreachable`
  looks like a forwarding bug when it is a socket that had not opened.
- **`Routes::any` cannot exist.** `every_registered_route_is_classified` panics on a method that is not
  GET/HEAD/POST/PUT/PATCH/DELETE, `no_get_route_is_mutating` forbids declaring a forwarded GET mutating, and
  `Routes::head` on a path `get` already answers panics on the overlapping method route. A method-agnostic route is
  therefore **five registrations of one path**, `get` read-only and the four mutating verbs `Mutating` — and that split
  is the honest one, because a forwarded `GET` cannot change state at *this* machine.
- **The forwarding route is not guard-exempt, and must not be.** A forwarded non-`GET` is checked against the **hub's**
  origin and client header first and only then re-authored as the peer's; reading the rewrite as "the hub trusts the
  caller's origin" would delete the hub's own guard. The peer's `owner` check is vacuous for a forwarded call (no
  `tailscale-user-login` arrives), which is HS-070's to change.
- **A `hosts.toml` row may name loopback where `POST /api/hosts` may not.** The seed is filtered by `name_problem` and
  `Origin::parse` alone (ADR-0073 D9), and that asymmetry is the only reason two machines can be measured on one box
  with no tailnet. Do not "fix" it.
- **A forwarded answer keeps the peer's `Content-Length` and never its `Transfer-Encoding`.** The first is what makes
  `cmp` of a forwarded row against the server's own row a real measurement; the second would be a second framing
  under hyper's. A response that loses its length is a truncation the client can see, and `hyper` aborts the
  connection — which is D7's "a close, then a sentence", not a hang.
- **A bound-and-dropped ephemeral port is a race, and it hangs rather than fails.** `egress`'s "a tunnel nothing
  answers" test used `bind(0)`, `local_addr()`, `drop`, then connected — and another test in the same binary binding
  that port in the window made the connect *succeed* against a listener that never answers, so `read_to_string` waited
  forever (`wchan` was `skb_wait_for_more_packets`) and HS-069's gate sat in `cargo test` for fourteen minutes with no
  output. The fix is the one the test has now: **port 0 is the port nothing can listen on** (a bind with 0 is an
  ephemeral port, so no process holds 0) and a connect to it is refused at once — plus a bounded read, so the next
  surprise is a failure with a sentence rather than a gate that cannot finish.

### Landmines: machine trust, and being a peer on one box (HS-070)

- **`127.0.0.2` is the whole measurement.** Without root, binding the *client* to `127.0.0.2` is the only way to be an
  address other than `127.0.0.1` — and that is not a trick, it is the rule: `tailscale serve` (deploy step 6) proxies
  both origins to `127.0.0.1`, so the front door is `127.0.0.1`/`::1` **exactly** and the rest of `127/8` is a peer.
  `IpAddr::is_loopback()` is the wrong predicate here by one bit of precision, and the gate would measure nothing if
  it were used (`scripts/verify.sh`'s `case_machine_trust`, `tests/trust.rs`).
- **A `TempDir` label is a directory, and `TempDir::new` empties it.** `tests/trust.rs`'s shim and its database both
  lived in `trust-arm` for a moment, and the boot wiped the shim. One label per artefact (`-whois`, `-db`), and every
  test names its own — two tests in one binary sharing a label is ADR-0009's same-process locking trap.
- **`App::new(router, guards)` served without `into_make_service_with_connect_info` has no address**, and an address
  nobody reported is a refusal when `allowed_hubs` is non-empty, never a pass. `serve::run` installs it; a test that
  drives the router in-process cannot, so that arm is a *unit* test (`Trust::admit(None)`).
- **The refusal's sentence says "hub" as the domain noun**, so a test or a case that asserts "no configured entry
  appears in the answer" must not put `hub` in `allowed_hubs` — it would not be able to tell a leak from prose. Both
  the unit test and the gate use `workshop`.
- **The banner is exactly eight lines at the default filter** (`tests/serve_cli.rs`), so the trust config is a
  `debug!` at startup and a `warn!` only per refusal. A startup `warn` would fail that test — and it did, in the
  first draft of the boot line.
- **`preflight::Tools` is the seam, so a new program joins it.** `tailscale` did — seven test literals name it and five
  more take `..Tools::default()` — and a `Trust`-only program path would have been a second seam for one idea.
- **`tailscale serve` rewrites the source address, and that is the whole topology question.** Deploy step 6 proxies
  `https://<machine>.ts.net` to `http://127.0.0.1:<port>`, so every caller — browser, hub, stray node — reaches the
  listener from `127.0.0.1`. The machine check therefore bites where the control origin is the listener's own
  tailnet-reachable address and **cannot** bite behind serve (ADR-0075 D10). Do not "fix" this by trusting
  `X-Forwarded-For` or `Tailscale-User-Login`: that is the caller-supplied claim item 4 forbids.
- **A forged identity header must forge a *listed* name to be a measurement.** `tests/trust.rs` and the gate both
  forge `workshop`, not an arbitrary login: with a name nobody lists, both an implementation that trusts headers and
  one that does not answer `403`, so the arm could not fail.
- **One whois per request on the tailnet path, and no cache.** A dev-server page is thirty children. That is recorded
  as a cost (ADR-0075 D8) rather than fixed: a cached admission outlives the tailnet's own membership change.

### Landmines: the Computers screen, and a page that knows whose session it is (HS-071)

- **`node --test` resolves `src` imports without an extension, so a module the tests import may not import an
  extensionless sibling.** Adding `COMPUTERS_PATH` to `transcript.ts` and importing it from `push.ts` was the obvious
  shape and it broke `npm test` with `ERR_MODULE_NOT_FOUND: …/src/transcript`: `test/push.test.ts` imports
  `../src/push.ts` **with** the extension, and node's ESM resolver then wants one on every hop. Vite and `tsc` accept
  the extensionless form, so `tsc --noEmit` was green and only `npm test` went red. The fix is not
  `allowImportingTsExtensions` (a build-wide semantic change for one import): the constant went to `push.ts`, beside
  `SETTINGS_PATH`, because the header's list of screens is the only thing that has to know all of them. **A shared
  constant belongs in the module both node and Vite already load first**, and `main.ts` (which nothing imports) is the
  only file that may import freely.
- **A card for a machine with no registry row still needs its "checked" fact.** The local card is drawn from
  `/api/health` and `/api/sessions` and has no `hosts` row behind it, so `reach` and `last_checked_at` have nothing to
  say — the first draft hid both, and the acceptance's "last health check" then had a hole in the one card that is
  always on screen. It is filled with `lastCheckText(secondsNow(), secondsNow())`: the page's own health answer *is*
  that check, and "checked just now" is the same word the rows use. `reach` stays hidden, because there is no probe.
- **An envelope's code belongs in front of a sentence only where the operator acts differently.** `hostFailure`
  keeps the code for a refusal about something the operator typed and drops it for the three registration codes
  (ADR-0076 D6) — but a **failed read** on a card (`upstream_unreachable`, `upstream_timeout`) shows the `message`
  alone: it already names the machine and the origin, and `upstream_unreachable: gone could not be reached at …`
  reads as a word to look up about a machine that is simply away. The demo caught the code before the review did.
- **`span:empty` in a flex row still costs a gap.** A machine that is not there has no role, version or uptime, and
  three empty spans with `gap: 0 0.75rem` pushed `unreachable at the last check` sideways — visible in the first
  screenshot. `.computer-facts > span:empty { display: none }` is the rule; `hidden` on each span is the same fix
  written nine times.
- **Two link texts with no separator read as one word.** The header drew `Sessions` and `Notifications` flush against
  each other (`SessionsNotifications`) once it had two links, because `.machine-nav` had no layout. A `display: flex;
  gap: 0 0.75rem` is the whole fix, and the screenshot is what found it — no gate can.
- **A version skew cannot be manufactured on one box by writing a row.** The card shows the version the *hop* reports,
  so `UPDATE hosts SET version = …` changes nothing on screen (that is ADR-0076 D1's point). What measures the arm end
  to end is a **stand-in**: a thirty-line `python3` socket that answers `/api/health` with `"version":"0.0.1"` and
  `/api/sessions` with one `stopped` row, seeded as a registry row like any other machine. Two facts make it work —
  the seed is not filtered by the host predicate, so the stand-in may be `http://127.0.0.1:<port>` (ADR-0073 D9), and
  the probe asks only `/api/health`, so nothing else has to be truthful.
- **The frame path's `closest("#session-list")` became a class.** The stopped-session window reaches `applyStatus`
  through the list element's `data-stopped-ttl-days`, and there is now one list **per machine** — so the id became
  `class="session-list"` plus `data-host`, and `addSession` finds the local group by
  `.session-list[data-host="local"]`. An id would have had to be duplicated per group, and `querySelector` would then
  answer the first machine's window for a row on another machine.

### Landmines: the CLI as a client, and the deploy's two halves (HS-072)

- **`deploy::Kind` collided with a private `Kind` that was already in `deploy.rs`** (the `Item` kind: `Directory`,
  `Rendered`, `Kept`, `Vapid`). Rust reports it as a duplicate definition, but the *first* symptom was 40 lines of
  "expected `holosmith::images::Kind`, found `holosmith::deploy::Kind`" in `tests/deploy.rs`, because that file
  imports `images::Kind` for the build expectations. The private one was renamed `ItemKind` (its honest name: it is
  an `Item`'s kind) and the test's import became `Kind as ImageKind`. **A new public type called `Kind` in a crate
  that already has two is worth checking for a third before writing it.**
- **`http::hosts` was a private module, so a client of the route could not name `HostList`.** The fix is
  `pub(crate) mod hosts;` plus `Deserialize` on the wrapper and on `Host` — one wire shape with one definition.
  `Reach` needed a **hand-written** `Deserialize` beside its hand-written `Serialize` (its words are `Reach::parse`'s,
  so a third word is the same sentence on the wire as in the table). The alternative — a client-side `List` struct —
  is a second spelling that drifts the first time the route grows a key. `ErrorBody` **cannot** be reused for reading:
  its `code` is a `&'static str` (its whole point on the server side), so the client has a two-field `Envelope` with
  owned strings, and the doc comment says why that is not a fork.
- **A `GET` may carry `Origin` and `X-Holosmith-Client`; it may not carry `Content-Type`.** The guard reads the
  content type as the claim "this request has a body", and a `GET` with one and no body is a lie the `bad_client`
  check does not read but that no client should send. The CLI sends one head for all three calls, with the content
  type only on the non-`GET`s, and `tests/hub_cli.rs` pins both halves — asserting the absence of a header is as much
  a measurement as its presence.
- **The name in a path segment has to be percent-encoded even though the alphabet refuses it.** `DELETE /api/hosts/a b`
  is a malformed request line, so the answer is a connect-level error instead of `422 bad_host_name` — the client
  cannot be told "that name is not one a row may take" by a server that never parsed the request. `segment()` encodes
  everything outside `[A-Za-z0-9-._~]`, and it is deliberately **not** a copy of `name_problem`: the route's alphabet
  is still the route's, and this only makes the name sendable so the route can refuse it.
- **A `grep` pattern got written with a `/` where a `.` belonged and the failure read as "the file is missing".**
  `"$WORK/$name.union.out"` became `"$WORK/$name/union.out"` in one of four identical-looking lines, so the run
  failed with `grep: …/case_hub_deploy/union.out: No such file or directory` while the file sat there with a dot in
  its name — and the printed `$(cat …)` in the same `fail` *worked*, which is the tell. `od -c` on the source line is
  what settled it: `sed`/`grep` rendering of the line showed the dot, so read the bytes before believing a diff.
- **`bash -x` on the whole gate is the fastest way to find which statement failed.** 2000 trace lines, greppable for
  the arm's own name (`grep -n union trace.out`), and it prints the **expanded** path — which is the only thing that
  showed the slash above.
- **A deploy half's two skips are `Word::Skipped` and the run continues; the *loop* must not treat them as failures.**
  `run_as` pushes the skip line and `continue`s before the step match, so `Kind::skip` is asked once per step and the
  remaining steps still evaluate. A `break` there would leave a hub's steps 5–7 unevaluated and the report would
  describe four steps of five.
- **Step 2's docker group is a `Kind` parameter all the way down** (`Kind::group` → `install` → `read_user` →
  `ServiceUser::pending` → `Action::UserAdd::group: Option<String>`). Skipping only the `usermod` would still ask
  `id -nG` and plan a `usermod` for a group the hub never uses; skipping only the `useradd` flag would create the user
  in the group the run was supposed to avoid.
- **`satisfied` on step 2 must not claim a hub's user is in no group.** Nothing removes a membership, so on a `both`
  machine that ran `server deploy` first the user *is* in the docker group — the clause says "exists and joins no
  group" for exactly that reason.
- **A `--root` deploy of the hub half needs no daemon, no `_ping` stub and no `health_responder`** — step 1 asks
  `doctor` about `Kind::Hub`, so the Docker and git checks are skipped and the four container questions are `unknown`
  with the hub sentence. That is what makes `case_hub_deploy` the only case in the run that needs neither `docker` nor
  an image; the *port* still has to be answered for step 7, and `serve` on that port is the honest stand-in. The
  machine's own `openssl` is **not** dispensable — see the bullet at the end of this section.
- **A `409 host_busy` arm needs a seeded local row, and the seed needs the port before the boot.** `POST /api/hosts`
  refuses loopback (`host_problem`) while `hosts.toml` is not filtered by that rule (ADR-0073 D9), and
  `hosts::is_local` compares hosts (so `http://127.0.0.1:<p>` is *not* this machine when `control_origin` is
  `http://localhost:<p>`). The seed's URL must be the control origin verbatim, and the boot has to be pinned
  (`start_serve <config> <data> <port>`) because `--port 0` re-derives the origin after the merge and the shortcut
  cannot fire.
- **`status`'s `!rendered.contains("machine ")` is a false assertion** — `machines 0` contains it. A row line starts
  at a line start, so the assertion is `"\nmachine "`. The suite caught it immediately, but the shape is worth
  remembering: a substring check over a report whose keys are prefixes of one another proves nothing.
- **The two-configs landmine applies to `status`/`hub host` as much as to `status`/`logs`.** The config a `--root`
  deploy reads has no `data_root` (so `--root` can prefix the default), and the config `serve`/`status`/`hub host`
  read has the prefixed one. One file cannot be both: `hub deploy --config A` then `status --config A` would read the
  machine's real `/var/lib/holosmith`.
- **A hub is exempt from the Docker and git checks and from nothing else — `openssl` included.** The tempting
  extension of `doctor::run`'s `Role::Hub` carve-out (`deploy.rs`'s `Kind::group`/`skip`, `doctor.rs:209`) is to drop
  the `openssl` finding too, because a hub runs no turns and sends no notifications. It is wrong: `hosts::probe` runs
  `s_client` for every `https` registry row, so a hub **without** `openssl` cannot reach a peer at all — the
  reachability of every machine on its Computers screen. So `hub deploy`'s step 7 asks it, and the docs must not claim
  a Docker-less machine is an everything-less one: `case_hub_deploy` needs `install`, `chmod`, `df` and `openssl` for
  step 2's VAPID pair and step 7's finding, and "needs no daemon and no image" is the whole of the headline.
- **A scripted Markdown insert that anchors on a link's *first line* eats the summary that follows it.** The ADR index
  writes each bullet as `- [ADR-00xx](path) — **the decision**, …`, and a replacement that matched only
  `- [ADR-00xx](path)` put the new bullet between the link and its own em dash — so ADR-0076 lost its description and
  ADR-0077 was given it. The gate cannot see a document, and only a review caught it. **Anchor on the end of the
  bullet** (`(HS-0nn).` is the shape every one of them ends with), then read the two lines back; this is the fourth
  time a scripted doc edit has cost a finding, and the existing rule ("read the changed lines") needed the *shape*
  of the mistake named.
- **`run_deploy` was a one-line delegation with a doc comment claiming it decided something.** A wrapper whose whole
  body is `deploy::run(args)` and whose `Kind` was chosen by its caller is a "Middle Man": `main` dispatches
  `HubAction::Deploy` straight to `commands::deploy::run`, and `commands::hub` keeps only the registry half. Worth the
  reflex — *does this function choose anything?* — because a doc comment that describes a decision nobody makes is
  worse than no wrapper at all.

### Landmines: the ssh transport, and what a copy needs (HS-073)

- **A `local` with no assignment is *unset*, and the EXIT trap still sees the failing function's locals.** `verify.sh`'s
  `cleanup` reads `$holder_pid` under `set -u`, so a case that declares `local … holder_pid` and then calls `fail`
  turns its own assertion into `line N: holder_pid: unbound variable` — the real `FAIL` line is *above* that in the
  log, which is the only reason it is findable. `case_deploy_ssh` assigns `local socket_pid="" holder_pid=""`; do the
  same in any new case, because every case that holds a port declares one of them.
- **An `ssh` stand-in must consume `-o` *and its value*.** A skip loop that only drops arguments starting with `-`
  takes `BatchMode=yes` for the host and hands the target's shell a command beginning with `-o`; dash answers
  `sh: 0: Illegal option -`, which reads like a broken stand-in rather than a broken parse. ssh's grammar is `-o X`,
  `-p X`, `-i X` and `-l X` with a value, and `-t`, `-T` and `-v` without.
- **`$HOME` is expanded by the *target's* shell, and the display form is not the token.** `remote::Sent` carries both:
  `token` (`$HOME/.cache/holosmith/deploy-…`, the word the copy, the check and the run all use) and `shown`
  (`~/.cache/…`, what the report prints). One field holding the display form made the run child's first word a `~`
  that the stand-in could not match — while a real target *would* have expanded it, so the bug was invisible outside
  a test. Two spellings of one path is the correct shape here, and the doc comment says which is which.
- **`/tmp` is often `noexec`, and a copy is not executable until someone chmods it.** The copy lands under the
  target's `$HOME/.cache/holosmith/`, and its shell string ends with `chmod 0755`: `cat >` creates 0644, so without
  that word the run child fails with `Permission denied` one step later, which names the wrong thing entirely.
- **`e_machine` is at offset 18, so a fixture header needs `e_type` first.** The ELF header is `e_ident` (16 bytes),
  `e_type` (2), then `e_machine` (2); a test helper that put the machine word at 16 makes the reader answer
  `ELF machine 0` — a failure that reads like a reader bug. `remote::elf_machine` also refuses a big-endian header
  rather than reading it backwards.
- **`Command::output()` gives the child `/dev/null` on stdin.** The copy child is `.stdin(Stdio::from(file))` —
  explicit, and it wins over `output()`'s default — while the run child inherits all three channels, which is what
  makes "streaming the output" literal rather than re-printed.
- **`--ssh`'s value cannot start with `-` on a command line anyway.** `clap` refuses `--ssh -oProxyCommand=…` as an
  unknown argument (exit 2), so `remote::host_problem`'s leading-`-` rule is reachable only through the `=` form
  (`--ssh=-oProxyCommand=…`, exit 1). The rule still earns its keep there, and the gate's arm spells it that way.
- **`grep -o 'deploy-[0-9a-f]*'` matches the bare prefix.** `*` allows zero characters, so counting unique copy
  names over the stand-in's log counted two (`deploy-` and the real name). The pattern needs `\{12\}`; a counting
  assertion over an `-o` grep is worth reading twice, because it fails as "the name is not stable" rather than as
  what it is.
- **`curl`/`wget` are not on any path this deploy builds, and that is the only honest form of "downloads nothing".**
  The gate puts stand-ins for both first on `PATH` that record and fail; the assertion is that the record file does
  not exist afterwards. Grepping the source for a downloader would assert what the text says rather than what the run
  did.
- **ssh hands the target's login shell *one string*, so a path is re-parsed there.** `--config` and `--root` are argv
  elements of `Command::arg`, but ssh joins the words after the host with spaces and the target's shell reads the
  result: `--root /srv/a b` arrives as `--root /srv/a` plus `b`, `/srv/*` arrives as whatever it matched, and `;`
  arrives as a second command — while the *same* command run locally needs no such care, so nothing shows it until a
  path has a space in it. `remote::quote` wraps a value in single quotes when it contains anything a shell acts on
  (`~` included, because the *target* would expand it), and the copy token is the same rule applied to an expansion:
  `"$HOME"/.cache/holosmith`. The resume line quotes both values too, because it is pasted into a local shell and then
  travels through a second one. This was the review's one P1, and the shape is worth keeping: **"verbatim" across a
  shell is a claim that needs a quoting function to be true.**
- **A child a signal killed has no exit code, and `unwrap_or(-1)` prints one that does not exist.** The remote run's
  line says `exited <n>` when there is a code and `was killed by signal <n>` when there is not; a report whose whole
  discipline is "a word means what it measured" cannot print `exited -1`.
