# Review — PER-42 (HS-010)

**Fixed point:** `dae1fd9` (the last commit before this work), reviewed against the **uncommitted**
tree. In-scope files read in full: `src/events/{stream,registry,mod}.rs`, `src/http/streams.rs`,
`src/http/{app,mod,error,sessions}.rs`, `tests/streams.rs`, `scripts/verify.sh`, `docs/build.md`,
`docs/adr/0014-*`, the `GLOSSARY.md` diff, and the `Cargo.toml`/`Cargo.lock` edges.

**Verification basis.** This role has no shell, so it did not execute the gates; it read the diff
artifact and the tree. The supervisor ran, after sourcing the toolchain env: `cargo fmt --check`;
`cargo clippy --all-targets -- -D warnings`; `cargo test` (14 binaries, 0 failed); `scripts/verify.sh`
end to end (exit 0, 17 cases + the CORS sweep, 51 assertions) — before and after the fixes below.

**Verdict:** Standards OK with notes (one should-fix, four nits). **Spec BLOCKED on one finding**, plus
one should-fix and two nits. The blocker was real, and it was the supervisor's planning error rather
than the implementer's: the plan specified the replay/live ordering carefully and never said the live
half needs the same per-session filter the SQL replay gets.

## Standards

Reviewed against ADR-0007/0008/0010/0013/0014, `docs/build.md`, `GLOSSARY.md`, and the smell baseline. `cargo fmt`/`clippy`-enforced matters were not re-checked.

**Correct**
- Guard layer untouched: the route is `Routes::get`, so `RouteKind::ReadOnly` and the `Host` guard apply with no exemption (`crates/holosmith/src/http/app.rs:302-310`; `is_guard_exempt` still names only `/api/health`). `tests/guards.rs:769,785` are generic over the manifest, so the new entry is covered.
- Unknown session answers ADR-0008's envelope before the registry is touched (`http/streams.rs:33-37`, `known()` at `:58`; proved by `tests/streams.rs:608`). The DB lock is still never held across an `.await` (ADR-0013): `attach` reads inside a sync block, `stream.rs:107-114`.
- `Cargo.lock` is exactly one edge: `Cargo.lock:305` adds `"futures-core"` to the `holosmith` package and nothing else; `futures-core 0.3.34` (`Cargo.lock:246`) and `futures-util 0.3.34` were already packages. Declared `Cargo.toml:18` + `crates/holosmith/Cargo.toml:16` as `.workspace = true`.
- Doc updates are proportionate: `GLOSSARY.md:155-169` adds four entries with no duplicate of the existing **Event writer** entry (`:152`), and the new **Stream** entry explicitly disambiguates it from `EventStream`. No stale case counts left in `docs/build.md`.

#### Finding — should-fix: ADR-0014 names "the client disconnecting" as a release path, but an idle disconnected stream never releases its count.
`StreamGuard` correctly lives in the spawned task and `Drop` is the only release (that part of the design is right, `registry.rs:135-139`). But the task is detached, and its only disconnect signal is a *failed send*: the loop blocks in `select! { guard.closed(), subscription.recv() }` (`stream.rs:147-150`). A client that vanishes while the session is silent (no event, no `close`) leaves the task parked forever with the guard alive — count, `sessions()` entry and `closers` slot retained until some event is appended anywhere or the process exits. ADR-0014 ("the guard lives in the stream's task, so every way a stream can end — the client disconnecting (the send fails)…") and the comment at `stream.rs:116-117` ("every way the task can end — the client gone…") overstate this.
Why it matters: spec item 11 has HS-066 suppress a push while a stream is open, and the stale count suppresses exactly the notification that would tell the operator the tab died; repeat offenders also grow `closers` unboundedly.
Smallest fix: add the receiver-gone branch to the idle select —
`() = items.closed() => return,` alongside `guard.closed()` (`stream.rs:147-150`); `mpsc::Sender::closed` is the same signal the send path already relies on. Test: connect, `drop(stream)` without reading, `wait_until(|| registry.count(&id) == 0)`.

**Finding — nit: duplicated 500-helper.** `internal()` (`http/streams.rs:82-91`) is the same shape as `http/sessions.rs:255-263` (log with `operation`, return the fixed sentence); only the log string and the error type differ (`impl Display` vs `RepoError`). Baseline: Duplicated Code. Smallest fix: one `http::error`-level helper taking `operation` + `log message` + `impl Display`, called from both.

**Finding — nit: public surface widened beyond need.** `http/mod.rs:10` adds `pub mod streams` next to a private `mod sessions` (`:9`), and `respond` is `pub` (`streams.rs:33`) — the handler becomes reachable from outside the crate only so `tests/streams.rs:473` can read `KEEP_ALIVE_INTERVAL`. Smallest fix: `mod streams;` plus a `pub use streams::KEEP_ALIVE_INTERVAL;` (or `pub(crate)` handler + re-exported constant).

**Finding — nit: `StreamRegistry::sessions()` (`registry.rs:68`) has no production caller** — only tests use it; the acceptance only asks for "SSE open on this session" (i.e. `count`). Baseline: Speculative Generality. Smallest fix: delete it and assert emptiness via `count`, or record it as HS-066's intended read in the ADR (currently the ADR only claims HS-066 reads `count`/`sessions` together).

**Finding — nit: `scripts/verify.sh:201` drops the quotes its siblings use** (`exec {fd}<>/dev/tcp/127.0.0.1/$port` vs `verify.sh:146,171`). Harmless today; it is drift in a file whose two other helpers agree.

**Verdict (Standards): OK with notes.** Nothing here blocks; the standards work (single layer, envelope, lock discipline, one-edge lock diff, glossary) is careful. Fix the stale-count release path before HS-066 reads it.

## Spec

Sources: PER-42 acceptance (5 boxes + demo), HS-010 in `docs/stories/phase-1-session-lifecycle-stub.md:65-78`, `HolosmithSpec.md` item 5 / item 15 (line 258), `/tmp/holosmith/plans/PER-42.md` D1–D8.

**Correct**
- Replay/live ordering in `attach` is right: `writer.subscribe()` at `stream.rs:106` precedes the history read at `:111`, and the skip is `if event.seq <= last_sent { continue; }` (`:163-165`) — the right direction (subscribe-first can only duplicate). The dedup rule itself is proved deterministically by `an_event_in_both_the_replay_and_the_subscription_arrives_once` (`stream.rs:243-269`).
- Lag path emits exactly one frame then ends: `Err(RecvError::Lagged(skipped)) => { hand(…Item::Lagged{skipped}).await; return; }` (`stream.rs:155-158`), and `Event::default().event(STREAM_LAGGED).data(…)` carries **no** `id` (`stream.rs:74-77`), so the browser keeps its last real seq and the reconnect replays the gap. Socket proof: `tests/streams.rs:411-467`.
- Heartbeat is axum's alone: `Sse::new(body).keep_alive(KeepAlive::new().interval(KEEP_ALIVE_INTERVAL))` (`http/streams.rs:44-46`), `KEEP_ALIVE_INTERVAL = 15s` named and public (`:29`), and there is no `Event::comment` anywhere in the crate (only the comment saying so, `:137`). The 300 ms `quiet_for` assertion (`tests/streams.rs:485-489`) is the right shape for "nothing else writes".
- `Last-Event-ID: n` → `n+1` (`read_since` is `seq > ?2`, `events/repository.rs:20-21`; `resume_point` at `http/streams.rs:71`) proved at `tests/streams.rs:321`; without it, from the beginning (`:294`). Unknown session → 404 (`:608`). Registry registration (`:538`) and clean close (`:496`) both proved.
- `verify.sh`: `fetch` and the earlier cases untouched; `fetch_stream` bounds every line at 3 s and stops at the blank line (`verify.sh:186-207`), so an SSE answer no longer hangs the 10 s `fetch`.

#### Finding — blocker: the live half is not filtered by session, so one session's stream receives every session's events.
```text
stream.rs:115-119   forward(subscription, &history, &items, &mut guard).await;   // no session id
stream.rs:146-172   loop { … Ok(event) => event … if event.seq <= last_sent { continue; } … }
```
The writer has **one** broadcast channel for all sessions (`events/writer.rs:33`, `:76-82` `let _ = events.send(event)` per event), and the replay is filtered by SQL (`WHERE session_id = ?1`, `events/repository.rs:20-21`) while the forward loop never compares `event.session_id` to the stream's session — `forward` is not even given `session_id`. Consequences, both real once HS-011 produces events (`max_live_sessions` defaults to 3, `config/mod.rs:220`):
1. Session B's `message.delta`/`session.status` frames are written into session A's SSE response (no session field in the frame, so the client cannot detect it) — cross-session content leak, contradicting ADR-0014's "one session's committed events".
2. `last_sent` is a single scalar: a foreign event with a larger seq sets `last_sent` past A's own next seq, so A's later events are silently dropped by `seq <= last_sent`, and the stream is thereafter out of order.
Why the suite is green: every test in `tests/streams.rs` creates exactly one session (`:272,297,324,354,387,414,541,580`), while `tests/events.rs:296-322` already proves two sessions interleave on the one channel.
Smallest fix: thread `session_id` into `forward` and skip foreign events before the seq check —
`if event.session_id != session_id { continue; }` at `stream.rs:160`. Test: open a stream on A, append A1, B1…B3, A2, assert A sees `[1, 2]` (this also catches the `last_sent` corruption), and `quiet_for` on a B-only append.

#### Finding — should-fix: the story's headline invariant (subscribe → read) has no test that fails if the two steps are swapped.
`an_event_committed_during_the_replay_window_is_neither_lost_nor_duplicated` appends **before** the stream attaches — `append(&state, &session, "second").await;` at `tests/streams.rs:592`, then `Sse::connect` at `:593` — and relies on the 500 ms commit window (`:577`) to land the commit inside the handler's window. So it appends neither during nor after: the commit's position relative to `subscribe`/`read_since` is a race, and the assertion `[1, 2]` holds under *either* ordering (commit before the read → replay carries it; after both → live half carries it). A `read`-then-`subscribe` regression therefore still passes. The unit tests do not close the gap either: they call `forward` with a subscription and snapshot handed in (`stream.rs:249-269`), so `attach`'s own order (`:106` vs `:111`) is untested. This is the one invariant HS-011 builds on, so the gap should not ship.
Smallest fix (deterministic, no timing): give `attach` an injectable read step, e.g. `attach_with(writer, registry, session_id, after, read: impl FnOnce(&str, i64) -> Result<Vec<Event>, RepoError>)`, and in the test pass a closure that sends an event on a `broadcast::Sender` clone *and then* returns `[event(1)]`. Under subscribe-first the follower receives it; under read-first it is lost. Then `cargo test --test streams` plus `cargo test --lib` must stay green.

**Finding — nit: the lag frame's missing `id` (the whole recovery mechanism) is not asserted.** `tests/streams.rs:441-451` asserts the kind and the `skipped` payload and that the stream ends, but not `last.id.is_none()`; nothing reconnects after a lag either. One added assert pins the rule that makes `Last-Event-ID` recovery work.

**Finding — nit: `last_sent` starts at `0`, not at the resume point (`stream.rs:136`).** With `Last-Event-ID: n` and an empty replay (history has nothing after `n`), the skip rule is vacuous, so a live event with `seq <= n` would be forwarded. Unreachable while seqs only grow, but it is one word from being self-consistent: pass `after` into `forward` and initialise `last_sent = after`. Folds into the blocker's edit.

**Verdict (Spec): BLOCK pending the session filter.** Every acceptance bullet has a corresponding implementation and test, the replay/live skip is written the right way round, the lag path is exactly one frame plus EOF with a recoverable watermark, the heartbeat is axum's alone, and the lock diff is one edge — but the live half forwards other sessions' events and corrupts `last_sent`, which no existing test can see.

Summary: Standards — 1 should-fix, 4 nits, no blockers (worst: stale registry count after an idle client disconnect). Spec — 1 blocker, 1 should-fix, 2 nits (worst: unfiltered live forward at `stream.rs:146-172`). Read-only: no files changed, no Linear calls; the only command a supervisor needs is `cargo test --test streams` after the fix and its test are added.
## Resolution

**One blocker, two should-fix and five nits: all fixed.** The blocker was genuine and the existing
suite could not see it — every stream test used exactly one session, while `tests/events.rs` already
proves two sessions interleave on the one broadcast channel.

| Finding | Status | How / why |
| --- | --- | --- |
| **BLOCKER — the live half is not filtered by session.** `forward` was not given the session id and compared only `event.seq <= last_sent`, while the writer has one broadcast channel for all sessions. So session B's frames were written into session A's SSE response (a cross-session content leak, undetectable by the client because the frame carries no session field), and a foreign event's larger seq pushed `last_sent` past A's own next seq so A's later events were silently dropped. Real as soon as HS-011 produces events, with `max_live_sessions` defaulting to 3. | **fixed, proved both ways** | `forward` takes `session_id` and `after`, drops a foreign event **before** the seq check, and starts `last_sent` at `after` rather than `0` (which also fixes the resume-point nit: with `Last-Event-ID: n` and an empty replay, a live event at or before `n` was forwarded). The socket test opens A, appends A1, then B1–B3, asserts `quiet_for(300 ms)` after B's three commit, then appends A2 and asserts A sees exactly `[1, 2]`. **With the filter removed it panics at `tests/streams.rs:401`, "another session's event was written into this stream"**, and the unit test yields `[1,2,3]` against `[1,2]`; with `last_sent = 0` the resume-point test yields `[2,3]` against `[3]`. Both restored, suite green. |
| **Should-fix (Spec) — the story's headline invariant had no test that fails if subscribe and read were swapped.** The existing test appended *before* the stream attached and relied on the 500 ms commit window, so `[1, 2]` held under either ordering and a `read`-then-`subscribe` regression still passed. | **fixed deterministically** | `attach` is a thin wrapper over `attach_with(…, read: impl FnOnce(&str, i64) -> Result<Vec<Event>, RepoError>)`. `attach_subscribes_before_it_reads` passes a read step that broadcasts seq 2 and *then* returns `[event(1)]`, so the stream must yield `[1, 2]`; under read-first seq 2 is lost. No timing involved. Production call sites and the lock discipline are unchanged. |
| **Should-fix (Standards) — a client that disconnects while the session is silent never released its registry count.** The guard is released by `Drop` in the spawned task, and the task's only disconnect signal was a failed send, so with no events and no `close` it parked forever. HS-066 reads these counts to decide whether to push, so a stale count would suppress a real notification. | **fixed** | A third `select!` branch on a named `RECLAIM_INTERVAL` (5 s) returns when the outgoing `mpsc::Sender::is_closed()` — exactly "the response body was dropped". It emits **no frame**, so the heartbeat stays axum's `KeepAlive` alone. `an_idle_stream_releases_its_count_and_writes_nothing` proves both halves with a 10 ms interval: nothing is written across 100 ms of ticks, and the count is zero after the body drops. ADR-0014's release-path paragraph now says how. |
| **Nit — duplicated 500 helper** between `http/streams.rs` and `http/sessions.rs` | **fixed** | One helper in `http/error.rs` taking the operation name and an `impl Display`. The client still never sees SQLite text or a path. |
| **Nit — public surface widened beyond need**: `pub mod streams` next to a private `mod sessions`, with `respond` public only so a test could read the keep-alive interval | **fixed** | `mod streams;` plus `pub use streams::KEEP_ALIVE_INTERVAL;`. |
| **Nit — `StreamRegistry::sessions()` had no production caller** | **fixed by deletion** | Speculative generality: `count` is what the acceptance asks for and what the tests use. HS-066 adds `sessions()` back when it actually reads it, which is the point at which its shape is known. |
| **Nit — `scripts/verify.sh`'s new `fetch_stream` dropped the quotes** its two sibling helpers use on the `/dev/tcp` path | **fixed** | Restored, so the three helpers agree. |
| **Nit — the lag frame's missing `id` (the whole recovery mechanism) was not asserted**, and nothing reconnected after a lag | **fixed** | The lag test now asserts `last.id.is_none()` — so the browser keeps its last real seq — and a reconnect after a lag shows the gap refilled by `Last-Event-ID`. |

After the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (14
binaries, all green — 178 lib, 14 streams) and `scripts/verify.sh` (exit 0, 17 cases + the CORS sweep,
51 assertions) pass, and `Cargo.lock` still shows exactly the one approved `futures-core` edge.
