# The abort ladder, how a turn is cancelled, and the second owner of a running turn

This is the story that closes the worst hole in the lifecycle: a turn that will not end. Before it,
`SessionBackend::abort`/`stop` were recordings, the turn runner awaited `start_turn` with nothing able
to interrupt it, and the stub's script was unconditional — so `running` had exactly one way out and a
hung agent had none. Five things had to be decided that the spec does not decide, and each is
expensive to change once (steer), (resume), (turn timeout) and (token
budget) have built on it.

## The backend reports how a turn ended; the runner still chooses the row

`start_turn` now answers `Result<TurnEnd, BackendError>` with `TurnEnd::{Settled, Aborted}`. This
looks like a state decision leaking back into the backend, and it is not: the backend reports what
happened to *its* turn — the agent finished, or something cut it short — and the runner maps that onto
the table, `agent_settled` or `abort_completed`, exactly as it maps an error onto `unexpected_exit`.
The server still owns every state change (ADR-0019); what changed is that the backend can now say one
fact the server cannot observe.

The alternative — leaving `start_turn` at `Result<()>` — would make the two rows of `running`
indistinguishable: an aborted turn would be written as `agent_settled`, and nothing in the log would
say an abort happened. The row is the API, and this story exists because "did the operator's abort
take effect" must be answerable.

## Cancellation is the backend's, and `clear_queue` is its own operation

Rung 1 is `clear_queue` then `abort` (spec item 16 §1). Both are trait methods of their own, and they
are the same two calls the spec names rather than one folded into the other: an agent whose queue
survives an abort starts the prompt the operator no longer wants, and a test can only assert that
ordering if it can observe both calls separately.

There is no cancellation token in the server, no `select!` around `start_turn`, and no per-session
registry of turn handles in `AppState`.

**Amended by ADR-0026.** `Turn::run` now races `start_turn` against its own deadline — that is a
`select!` around it — but nothing is cancelled by it: the deadline's arm hands the ladder a **task of its
own** (`Ladder::spawn`, the abort handler's entry point) and then keeps awaiting the turn, so rung 1 works
exactly as it does for an operator's abort and the backend still decides `TurnEnd`. The rule that matters —
no server-side token, and none of the server's belief about a turn the agent is still running — is
untouched; what changed is only *when* the ladder is asked. A token would let the server *believe* it had stopped a turn
the agent is still running, and the whole point of rung 2 is that the agent is not to be trusted to
honour anything. So the interruption is the backend's own: the stub's turn loop checks a per-session
stop flag between frames and answers `Aborted` instead of finishing its script; Docker's
implementation will answer when its agent process exits. The flag is consumed by the observation, so
a stop cannot leak into the next turn of the same session.

The hung agent is a **constructor argument**, `StubBackend::ignoring_abort()`, for the same reason
`failing_at(step)` and the pace are: a stub's misbehaviour is a test's business, and nothing an
operator can put in `config.toml` should be able to make abort lie. `serve --backend stub` therefore
always honours rung 1, and rung 2 is proved by `tests/abort.rs`.

## The ladder is one routine with a parameterised reason

`crate::ladder::Ladder` is built like `Provisioner` and `Turn`: `spawn` for production, `climb` for a
test. `climb(session, reason)` takes the reason as an argument, because the turn timeout and
the budget climb the same two rungs and record their own word — `turn_timeout`,
`budget_exceeded` — where the abort records `forced_abort`. A constant inside the routine would have
made the two later stories a copy of it.

**The parameter is rung 2's reason only.** Rung 1's ending is the turn runner's and is always
`abort_completed`, because the runner is not told why its turn is being stopped — the backend answers
`Aborted` and nothing more. That is enough for this story (an abort is what rung 1 is for) and it is
*not* enough and, whose acceptance is that the caller's own word reaches the row's
`status_reason`. Those two stories must plumb it: the smallest shape is a reason on the turn itself
(`Turn` holding the `&'static str` its abort ending records), set by whoever climbs the ladder. Naming
that here is deliberate — a later story that finds the row saying `abort_completed` after a timeout has
this paragraph to blame, not a mystery.

The rungs, in the spec's order and with the reasoning that fixes each one:

| Step | What it does | Why in this order |
| --- | --- | --- |
| 1a | `clear_queue` | The queue is what would run *next*; dropping it first means the abort cannot be followed by the work it was meant to cancel |
| 1b | `abort` | The agent's own stop, which is the cheap path |
| 1c | wait until `abort_grace` has passed, reading the row | The row is the question, not a flag in the ladder: the runner writes `running → idle` when the agent stops, so "the row is no longer `running`" *is* "rung 1 worked" |
| 2a | apply `abort_ladder` → `stopped` with the reason | The row first, because the other order leaves a row that says `idle` over a dead container — the one lie a client cannot detect |
| 2b | `stop` the backend | Kills the hung agent, which is what finally makes the turn answer `Aborted` |
| 2c | apply `resume` → `starting`, then the provisioner | "Then immediate resume" (item 16 §2); the four steps are the provisioner's, and that is the path the endpoint reuses |

`stopped_at` moves with the state inside the same write (ADR-0016), so a forced stop is instantly
eligible for the idle stop and the retention without a second statement.

The ladder does **not** call `SessionBackend::resume`. Whether a resumed session runs all four
provisioning steps or only the container's is a later question — the trait's `resume`
exists and has no caller until a change says what it means. Answering it here would have been a
guess that then had to undo.

## The ladder ends the turn it was asked about, and no other

**Amended: it may only end its own turn.** The ladder is spawned and then returns, so a climb outlives the turn
that asked for it — and every question it used to ask was about the **session**. Rung 2's `abort_ladder` is
legal from `running` whatever turn is running, and rung 3's `resume` provisions whatever row is there, so a
prompt sent just after a timed-out turn could have its container stopped mid-flight and the session restarted
with the row's reason naming a timeout the operator never hit. The window was the whole climb — up to
`abort_grace` plus the forced rung — and it widens exactly when the machine is busy, which is when a turn is
most likely to have timed out. The operator's abort is the same routine and had the same window.

The row could not answer it, so the row gained the fact: `sessions.turn_generation` (migration `0008`, schema
8), a counter the prompt bumps **inside the same critical section as its `running` write**, so a turn's
identity and its `running` are one critical section and no climb can see the new turn's row under the old one's
number. They are two statements in it and not one transaction, which leaves the one failure window
Consequences records.
It is deliberately not a `Session` field — the session object is the API's body, pinned to fifteen keys
(ADR-0038) — and it is read together with the row's state (`SessionRepository::turn`), because "which turn"
and "which state" are one question.

The ladder carries that number. Rung 2's write is the one its own turn may make
(`Transitioner::apply_for_turn`): the table stays the authority for which moves exist, and the generation
narrows *whose* row it is — the comparison, the lookup and the write are one critical section, which is what
makes it a compare-and-swap rather than a check followed by a hope. And rung 1 is not asked of a row that is
not this turn's at all, because `clear_queue` and `abort` name no turn: they reach whatever the session is
running now. A climb that finds another turn's row answers `Ending::Superseded`, asks the agent nothing, stops
nothing, resumes nothing, and says so in one log line — which is how an operator learns that the abort they
pressed was for the turn before this one, instead of seeing a turn they never ended.

## Two owners can end one turn, and the table decides
Rung 2 exists precisely because rung 1 did not work, so the turn runner and the ladder both end up
holding the same `running` turn: the ladder applies `running → stopped`, and only then does the stop
it issued make the runner's `start_turn` answer `Aborted`, at which point the runner's own ending is
`abort_completed`, which the table now refuses.

That refusal is **not a bug and must not read like one**: it is the designed outcome of two owners
racing on one row. The runner therefore treats `StateError::Illegal` for the abort ending as a `debug`
line ("the abort ladder ended this turn instead"), and only for that ending. Everywhere else a
refused transition stays what made it — an error the caller has to be able to explain. This is
the same trade ADR-0019 accepted for a prompt that loses a race to another prompt: the table is the
authority, and losing a documented race is not a failure.

**Amended by the section below.** That reporting moved into `Transitioner::apply_for_turn_logged`, and a
refused ending is a `debug` line for all three of the runner's endings — `Illegal` (the ladder moved the row
first) and `Superseded` (a later turn holds it) alike — because every one of them is a race the runner is
allowed to lose. The provisioner's and the exit path's `apply_logged` still calls an `Illegal` an error.

The reason on rung 1's ending is `abort_completed` — the trigger's own word — because
`session.status` has no trigger field (ADR-0016) and the demo's claim is that "the reason says which
rung fired". A client therefore reads `reason: "abort_completed"` for a clean abort and
`reason: "forced_abort"` for one that had to kill the container. The row's final state clears it: the
resume's transition carries no reason, so a session that came back from a forced abort is `idle` with
`reason: null` and the log is where the history lives.

## The runner ends the turn it was handed, and no other

**Amended: what the ladder may do to a row, the runner obeys as well.** The narrowing above was written for
the climb, and the runner had the same hole one level down. Its three endings went through the unguarded
`Transitioner::apply`, and `agent_settled`, `abort_completed` and `unexpected_exit` are all legal from
`running` whatever turn holds the row — so a runner that outlived its turn wrote its ending onto the next
turn's row, and the row is the API: "turn 2 ended" is a lie no client can detect.

The case that reaches it is the ladder's own, one answer later. Turn 1 runs past its deadline, the forced
rung stops the container and resumes the session, a prompt makes the row `running` again for turn 2, and
only then does turn 1's `start_turn` answer `Aborted` — a backend is free to take its time about that,
because a container dies when it dies and not when the stop is asked. Turn 1's runner then writes
`running -> idle` with `abort_completed` on turn 2's row, after turn 2's prompt was accepted. The settle arm
does the same with `agent_settled`, and it has the worse half: the WIP snapshot it takes first is of the
worktree turn 2 is writing. The error arm writes `error` the same way.

So the runner obeys the generation the ladder does, and it already held it — `Turn` was handed it by the
prompt and passed it to `spawn_ladder`, and did not use it for its own ending. All three endings are now
`Transitioner::apply_for_turn`'s, so a row the runner no longer owns answers `StateError::Superseded` and
nothing is written, the event included.

**The snapshot is guarded separately, because the order is the spec's.** The WIP snapshot runs *before* the
ending (item 9), so the ending's compare-and-swap cannot cover it: it asks whether the row is still this
turn's — the generation alone — and skips the git command otherwise. The status is deliberately not part of
that question. It is the climb's: there the row must still say `running` for there to be anything left to
wait for, which is why `Whose::Mine` answers "should I keep waiting". Here the harm is a worktree another
turn is writing, and the generation alone answers that. A row that left `running` without the generation
moving is not another turn's at all — the exit classifier writes `pi_exit` or `unexpected_exit` from
`running`, between the agent's last frame and its `finished` — and that turn's work is exactly what the ref
exists to mark. A read narrows that window and does not close it; what keeps the *row* right is still the
ending's CAS, and the residue is a ref of a worktree that stopped being this turn's in the moment between
the two. That is why this is written as a narrowing and not as an invariant.

## `409 session_idle` joins the family

The refusal family ADR-0019 established is `409 session_<state>`, spelled from `Status::as_str`. An
abort needs it for `idle`, which ADR-0019 could not express: `not_idle(Idle)` returned the fixed 500
because a prompt is *accepted* in `idle`. So the constructor became
`not_in_state(status, message)` — total over the six states, one code each, `session_idle` included —
and the **message** is the caller's, because a sentence has to describe its own request: an abort's
refusal says "the session is not running, so it has no turn to abort", a prompt's says "the session is
not idle, so it cannot be prompted". needs `session_idle` for steer and gets it for free.

An abort on anything but `running` is refused rather than answered `202` with no work. The spec's
family rule and the "on `idle` it returns `409 session_idle`" make a silent 202 the odd one out,
and a 202 that does nothing is indistinguishable from a ladder that failed.

## Consequences

- `crates/holosmith/tests/abort.rs` proves: the 202's row is still `running`; rung 1's calls are
  `clear_queue` then `abort` with no `stop`; the turn ends `running → idle` with reason
  `abort_completed`; a hung stub (`ignoring_abort`) reaches `stopped`/`forced_abort` and comes back
  `idle` with the same id and the four provisioning steps; the ladder's reason is the caller's
  (`turn_timeout`); `409 session_starting`, `409 session_idle` twice over, and `404 not_found`; and
  the route is recorded `Mutating`. Every refusal is asserted on status, `code` and `content-type`.
  **An abort for a turn that has already ended** answers `Ending::Superseded` and asks the agent for
  nothing, asserted there and, for a timed-out turn's climb, in `tests/turn_timeout.rs` — the row and
  the next turn's container are the two it must not touch.
- `src/backend/stub.rs` grows one test in three phases: an abort cuts the script short, an ignored
  abort does not, and a stop cuts it either way and leaves no stop request behind.
- **A runner's ending is its own turn's.** `tests/turn_timeout.rs`'
  `a_timed_out_turns_runner_does_not_end_the_turn_that_followed_it` drives the interleaving: turn 1's climb
  forces the stop and resumes the session, turn 2's prompt takes the row, and only then does turn 1's runner
  answer — turn 2's row is still `running`, with its own generation and no reason, no `session.status` event
  carries turn 1's ending, and no `Call::Snapshot` is recorded for turn 2's worktree. Both guards are
  load-bearing: with either one removed the test is red, on the row and on the call list respectively.
- **The prompt's two statements are one critical section and not one commit.** The status write and the
  generation bump happen under the one `Mutex<Database>` guard, so no in-process reader sees a `running`
  row at the previous generation; a failure of the bump after the status write has committed answers the
  prompt `500` with the row `running` at the previous generation and no runner started, which the abort
  ladder can still end. Making the pair one commit is the fix if that window ever matters.
- **A delete whose climb is superseded proceeds with the row as it stands**, snapshot included: the delete
  can only ask for the ending of the turn it read, so a prompt that took the turn in the meantime leaves the
  delete snapshotting and then removing a session that is running that new turn.
- `scripts/verify.sh` grows two cases (`case_abort`, `case_abort_on_an_idle_session`) — 28 cases —
  and the shipped artifact is where rung 1's `abort_completed` is proved, because rung 2's trigger is
  a constructor argument only a test can set.
- `web/src/main.ts` aborts for real now: a POST with no body and the two guard headers, a notice on
  success and the envelope's code on failure. The badge follows the stream, not the answer.
- `SessionBackend` is now ten methods. `steer` and `resume` had no caller when this was written;
  gave `steer` one (**see ADR-0021**) and a later change owns `resume`, which is why the methods were added with
  the trait rather than with their callers.
