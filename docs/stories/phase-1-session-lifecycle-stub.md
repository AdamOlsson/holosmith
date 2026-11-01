# Phase 1 — The session lifecycle against the stub backend

**Goal:** the entire session product — create, provision, prompt, stream, steer, abort, stop, resume, delete, evict, time out — working end to end with no Docker, no git and no agent process.

**Exit criteria:** from a browser you can create a session, watch it provision step by step, prompt it and see a live transcript, steer it, abort it, stop and resume it, delete it; events survive a server restart with no duplicate or missing seq; caps, eviction, idle stop, turn timeout and token budget all fire; every state transition goes through one enforced table.

This is the phase that makes the product feel real. Docker arrives in phase 1b and must satisfy this contract unchanged.

See ADR-0001 for why this comes before containers.

---

### HS-007 — Session row and repository layer

**As** the operator **I want** a session to exist as durable state **so that** everything else has something to hang off.

- **Size:** S · **Blocked by:** HS-004 · **Spec:** Data model and API changes
- **Demo:** a session row survives restart and reads back identically.

**Acceptance**
- [ ] Migration adds `sessions` with every column the spec names: `worktree`, `status`, `status_reason`, `client_key` (UNIQUE), `network` (default 0), `stopped_at`, `usage_in`, `usage_out`.
- [ ] Plus the columns the lifecycle needs and the spec implies: `id`, `project`, `model`, `thinking`, `base_commit`, `created_at`, `last_active_at` (for LRU eviction).
- [ ] A repository layer owns all reads and writes; no SQL outside it.
- [ ] Unit tests cover create, read by id, read by `client_key`, list with state filter, update state.

**Notes**
- `worktree` is `NOT NULL` per the spec but is empty until the provisioner runs; store `''` for stub and unprovisioned sessions rather than weakening the column.

---

### HS-008 — `POST /api/sessions`: 202, validation, idempotency, cap

**As** the operator **I want** to start a session and get an answer immediately **so that** a slow start never looks like a failed tap.

- **Size:** M · **Blocked by:** HS-007, HS-005 · **Spec:** item 7, API table
- **Demo:** `curl` twice with the same `Idempotency-Key` returns the same session id; a bad body returns 422; a full machine returns 409.

**Acceptance**
- [ ] Body is `{project, model, thinking, network}`; unknown fields are rejected; a bad project, model or thinking → 422 with a machine-readable code.
- [ ] The row is inserted in `starting` with the client's `Idempotency-Key` in `client_key`; a repeat of the same key returns the existing session with 202, not a new one.
- [ ] A missing `Idempotency-Key` → 422 (not silently accepted), because retries are the whole point.
- [ ] When the cap is reached and nothing is evictable → 409 `cap_reached`.
- [ ] Response is 202 with the session object, `Location` header, and the event stream URL.
- [ ] The endpoint does no provisioning work itself; it hands off and returns.

---

### HS-009 — Event log and the single writer with group commit

**As** the operator **I want** events to be durable before I see them **so that** a crash cannot rewind my session or make my client drop events.

- **Size:** M · **Blocked by:** HS-007 · **Spec:** item 5
- **Demo:** kill -9 the server mid-stream; on restart the client resumes with no duplicate and no missing seq.

**Acceptance**
- [ ] `events` table: `session_id`, `seq`, `type`, `payload` (JSON), `created_at`, unique on `(session_id, seq)`.
- [ ] Exactly one writer task receives events from all sessions; it commits every 25 ms in one transaction and only then broadcasts, in order.
- [ ] Seq is assigned inside the writer, strictly increasing per session, never reused.
- [ ] WAL and `synchronous=FULL` are asserted at startup, not assumed.
- [ ] Broadcast happens after commit, never before; a slow subscriber cannot delay the commit.
- [ ] A test asserts the observable rule: no client ever sees a seq that a crash could undo.

---

### HS-010 — SSE stream with `Last-Event-ID` resume

**As** the operator **I want** the transcript to reconnect without gaps **so that** switching apps on my phone does not lose output.

- **Size:** M · **Blocked by:** HS-009 · **Spec:** item 5, item 15
- **Demo:** two browser tabs show identical order; close one, reconnect, it continues at the right seq.

**Acceptance**
- [ ] `GET /api/sessions/:id/events` is a server-sent event stream; the SSE `id` is the seq.
- [ ] With `Last-Event-ID: n`, replay starts at `n+1`; without it, behaviour matches HS-061's rule (in this story: from the beginning).
- [ ] Multiple concurrent subscribers each get every event in order; a slow one is dropped with a `stream_lagged` event rather than blocking others.
- [ ] Heartbeat comment every 15 s so intermediaries do not idle-close.
- [ ] Stream closes cleanly when the session is deleted; deleting during an open stream does not panic.
- [ ] A subscriber is registered for "SSE open on this session" — HS-066 needs it later.

---

### HS-011 — Stub backend and provisioner steps

**As** the operator **I want** to see what a starting session is doing **so that** a slow start is legible instead of a spinner.

- **Size:** M · **Blocked by:** HS-008, HS-010 · **Spec:** item 7, `--backend stub`
- **Demo:** create a session and watch `worktree`, `network`, `container`, `pi` steps arrive, then `idle`.

**Acceptance**
- [ ] A backend trait covers: provision, start turn, steer, abort, stop, resume, delete, snapshot. Stub implements it with configurable per-step delays.
- [ ] `backend = "stub"` is selectable by config and by `--backend`; stub needs no Docker, git or agent binary.
- [ ] The provisioner runs in the background and emits `session.status` with `step` set to `worktree`, `network`, `container`, `pi`.
- [ ] On failure it undoes completed steps and sets `error` with a reason; partial resources are not left behind.
- [ ] Success ends in `idle`, not `starting`.
- [ ] The stub can be told to fail at a named step, and a test does exactly that.

---

### HS-012 — One enforced state machine

**As** the maintainer **I want** every transition to go through one table **so that** no future story can invent an illegal state.

- **Size:** S · **Blocked by:** HS-011 · **Spec:** item 8
- **Demo:** a test attempts each illegal transition and gets a logged refusal, not a corrupt row.

**Acceptance**
- [ ] The transition table from the spec is data in one place: `starting|idle|running|stopped|error|deleting` with exactly the listed triggers.
- [ ] Every state change calls one function; direct writes to `status` are impossible outside it.
- [ ] An illegal transition is a bug: it is logged with from/to/trigger and refused, never applied silently.
- [ ] `session.status` events carry from, to and optional reason; `status_reason` is persisted.
- [ ] `stopped_at` is set on entering `stopped` and cleared on resume, in the same function.
- [ ] The state `ended` does not exist anywhere; `interrupted` is a reason, not a state.

---

### HS-013 — UI: sessions list and the new-session form

**As** the operator **I want** to see my sessions and start one from my phone **so that** the product is usable at all.

- **Size:** M · **Blocked by:** HS-012 · **Spec:** item 2 (Internet access checkbox), item 8, item 15
- **Demo:** the list shows live states; a new session appears as `starting` and turns `idle` without a reload.

**Acceptance**
- [ ] Sessions screen lists id, project, model, state with reason, and last activity; live updates over the event stream.
- [ ] New session form: project, model, thinking level, and an **Internet access** checkbox defaulting from `network_default` (off).
- [ ] Create generates an `Idempotency-Key`, sends it, and shows `starting` immediately; a network failure and retry reuses the key.
- [ ] Provisioning steps are shown while `starting`.
- [ ] Stopped sessions show days remaining before auto-delete (wired to HS-064; placeholder until then).
- [ ] Mobile-first layout: one column, tap targets sized for a phone.

---

### HS-014 — UI: session screen with a live transcript

**As** the operator **I want** to watch the agent work **so that** I know whether to wait, steer or abort.

- **Size:** M · **Blocked by:** HS-013 · **Spec:** item 15, item 5
- **Demo:** prompt a session, watch text and thinking arrive token by token, background the tab, come back, transcript is complete.

**Acceptance**
- [ ] Session screen subscribes to the stream and appends deltas per message id; thinking is visually distinct from text.
- [ ] Reconnect uses `Last-Event-ID`; a `message.snapshot` replaces by message id rather than appending (safe even mid-delta).
- [ ] State badge, reason, and a per-turn boundary are visible.
- [ ] A composer exists (wired in HS-015) with Steer and Abort affordances shown only when `running`.
- [ ] Rendering stays responsive at 10k events: no full re-render per delta.

---

### HS-015 — Prompt and the turn lifecycle

**As** the operator **I want** to send a prompt and see a turn complete **so that** the core loop works.

- **Size:** M · **Blocked by:** HS-014 · **Spec:** item 8, item 13
- **Demo:** prompt the stub session, watch `idle → running → idle` and a scripted answer with thinking, text, a tool call and usage.

**Acceptance**
- [ ] `POST /api/sessions/:id/prompt` returns 202, moves `idle → running`, and rejects with 409 unless the session is `idle`.
- [ ] `agent_settled` moves `running → idle` and is the only normal way out of `running`.
- [ ] The stub's scripted turn includes thinking, text, a tool call and usage, so the UI exercises every rendering path.
- [ ] Turn start and end are explicit events, so the transcript has visible boundaries.
- [ ] `last_active_at` updates on prompt, on settle and on steer (drives HS-020 and HS-021).

---

### HS-016 — Abort with the full ladder

**As** the operator **I want** abort to always work, even if the agent hangs **so that** I am never stuck paying for a runaway turn.

- **Size:** M · **Blocked by:** HS-015 · **Spec:** item 16, API table
- **Demo:** abort a stub turn that ignores the first abort; it still ends, and the reason says which rung fired.

**Acceptance**
- [ ] `POST /api/sessions/:id/abort` returns 202 immediately; progress arrives as events.
- [ ] Rung 1: `clear_queue` then `abort` over the backend RPC.
- [ ] Rung 2: if not idle after `abort_grace` (10 s), force-stop → `stopped` with reason `forced_abort`, then immediate resume → `idle`.
- [ ] Abort completing normally moves `running → idle`.
- [ ] The ladder is a shared routine with a parameterised reason, because HS-022 and HS-023 reuse it.
- [ ] A test drives the stub to ignore rung 1 and asserts rung 2 fires and the session ends `idle`.

---

### HS-017 — Steer a running turn

**As** the operator **I want** to correct the agent mid-turn **so that** I do not have to abort and start over.

- **Size:** S · **Blocked by:** HS-015 · **Spec:** item 8
- **Demo:** steer while running, the guidance appears in the transcript and the turn continues.

**Acceptance**
- [ ] `POST /api/sessions/:id/steer` accepts guidance while `running` and returns 202.
- [ ] On `stopped` it returns 409 `session_stopped`; on `idle` it returns 409 `session_idle`, and the UI offers to send it as a prompt instead.
- [ ] The steer is recorded as an event so the transcript shows what was injected and when.

---

### HS-018 — Stopped state and resume

**As** the operator **I want** to bring a stopped session back **so that** an evicted session is not lost work.

- **Size:** M · **Blocked by:** HS-016 · **Spec:** item 8, item 9
- **Demo:** stop a session, see it `stopped` with a reason, resume it, and it returns to `idle` with the same id and full transcript.

**Acceptance**
- [ ] `stopped` is reachable from `idle` (evict, idle stop, drain) and from a killed `running` turn; it is never a dead end.
- [ ] `POST /api/sessions/:id/resume` returns 202 and goes `stopped → starting → idle` through the provisioner.
- [ ] Prompt, steer and abort on a stopped session return 409 `session_stopped`; the UI resumes and retries automatically.
- [ ] Resuming a session on a full machine evicts the least recently active idle session first, or returns 409.
- [ ] History, events and the worktree path survive the stop; only the runtime is gone.

---

### HS-019 — Delete flow

**As** the operator **I want** delete to be safe and reversible until it is not **so that** a mis-tap cannot destroy uncommitted work.

- **Size:** M · **Blocked by:** HS-018 · **Spec:** item 9, API table
- **Demo:** delete a session with a failing snapshot → 409 `unsaved_work`; retry with `?force=true` → 202 → gone.

**Acceptance**
- [ ] `DELETE /api/sessions/:id` returns 202, sets `deleting`, then removes the row; the stream closes.
- [ ] Abort first if running; snapshot before removal; remove session directory and network.
- [ ] A failed snapshot returns 409 `unsaved_work` unless `?force=true`.
- [ ] Delete is idempotent: a second call returns 202/404 deterministically, never a partial state.
- [ ] Deleting a session revokes its dummy key (wired in HS-039) — the hook exists here.

---

### HS-020 — Cap and eviction

**As** the operator **I want** a new session to win over a sleeping one **so that** I never have to clean up by hand to start work.

- **Size:** M · **Blocked by:** HS-019 · **Spec:** item 8, item 9
- **Demo:** with the cap at 1, creating a second session stops the idle first one and succeeds.

**Acceptance**
- [ ] The cap counts live sessions only: `starting`, `idle`, `running`. `stopped`, `error`, `deleting` do not count.
- [ ] Create at cap evicts the least recently active `idle` session → `stopped` with reason `evicted`, then proceeds.
- [ ] If nothing is evictable (all live sessions are `running` or `starting`) → 409 `cap_reached`.
- [ ] Eviction deletes nothing: history, worktree, events and images survive, and the UI says so.
- [ ] `max_live_sessions` is configurable and validated as ≥ 1.

---

### HS-021 — Idle stop

**As** the operator **I want** a session I forgot about to stop costing me **so that** an idle machine is not a bill.

- **Size:** S · **Blocked by:** HS-020 · **Spec:** item 13
- **Demo:** set `idle_stop_min` to a minute, leave a session idle, watch it stop and free its slot.

**Acceptance**
- [ ] A sweeper moves `idle` sessions whose `last_active_at` is older than `idle_stop_min` to `stopped` with reason `idle_stop`.
- [ ] It never touches `running`, `starting`, `error` or `deleting`.
- [ ] Stopping frees a cap slot immediately.
- [ ] The sweep is observable: a `session.status` event with the reason, and a log line per stop.

---

### HS-022 — Turn timeout

**As** the operator **I want** a turn that never ends to be killed **so that** a wedged agent cannot run forever.

- **Size:** S · **Blocked by:** HS-016 · **Spec:** item 13, item 16
- **Demo:** set `turn_timeout_min` to a minute with a stub turn that never settles; the ladder fires and the session returns to idle.

**Acceptance**
- [ ] A `running` turn older than `turn_timeout_min` is aborted using the HS-016 ladder with reason `turn_timeout`.
- [ ] The reason reaches the transcript and the session's `status_reason`.
- [ ] A settled turn resets the clock; the timeout is per turn, not per session.
- [ ] `0` disables the timeout and is documented as such.

---

### HS-023 — Token budget

**As** the operator **I want** a hard spend ceiling per session **so that** one runaway loop cannot drain my quota.

- **Size:** S · **Blocked by:** HS-022 · **Spec:** item 13, V3 (usage reporting)
- **Demo:** set `max_session_tokens` low; the stub turn trips it, aborts, and records `budget_exceeded`.

**Acceptance**
- [ ] `usage_in` and `usage_out` are updated from `usage` on `message.end` and persisted per session.
- [ ] At `max_session_tokens` the turn is aborted through the shared ladder with reason `budget_exceeded`.
- [ ] Unset means unlimited; the key is optional and absence is not an error.
- [ ] If the backend reports no usage, the budget is disabled and the UI says so — this is the spec's stated contingency, exercised with a stub mode that omits usage.
