# The abort ladder, how a turn is cancelled, and the second owner of a running turn

HS-016 is the story that closes the worst hole in the lifecycle: a turn that will not end. Before it,
`SessionBackend::abort`/`stop` were recordings, the turn runner awaited `start_turn` with nothing able
to interrupt it, and the stub's script was unconditional — so `running` had exactly one way out and a
hung agent had none. Five things had to be decided that the spec does not decide, and each is
expensive to change once HS-017 (steer), HS-018 (resume), HS-022 (turn timeout) and HS-023 (token
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
registry of turn handles in `AppState`. A token would let the server *believe* it had stopped a turn
the agent is still running, and the whole point of rung 2 is that the agent is not to be trusted to
honour anything. So the interruption is the backend's own: the stub's turn loop checks a per-session
stop flag between frames and answers `Aborted` instead of finishing its script; Docker's HS-024
implementation will answer when its agent process exits. The flag is consumed by the observation, so
a stop cannot leak into the next turn of the same session.

The hung agent is a **constructor argument**, `StubBackend::ignoring_abort()`, for the same reason
`failing_at(step)` and the pace are: a stub's misbehaviour is a test's business, and nothing an
operator can put in `config.toml` should be able to make abort lie. `serve --backend stub` therefore
always honours rung 1, and rung 2 is proved by `tests/abort.rs`.

## The ladder is one routine with a parameterised reason

`crate::ladder::Ladder` is built like `Provisioner` and `Turn`: `spawn` for production, `climb` for a
test. `climb(session, reason)` takes the reason as an argument, because HS-022's turn timeout and
HS-023's budget climb the same two rungs and record their own word — `turn_timeout`,
`budget_exceeded` — where the abort records `forced_abort`. A constant inside the routine would have
made the two later stories a copy of it.

The rungs, in the spec's order and with the reasoning that fixes each one:

| Step | What it does | Why in this order |
| --- | --- | --- |
| 1a | `clear_queue` | The queue is what would run *next*; dropping it first means the abort cannot be followed by the work it was meant to cancel |
| 1b | `abort` | The agent's own stop, which is the cheap path |
| 1c | wait up to `abort_grace`, reading the row | The row is the question, not a flag in the ladder: the runner writes `running → idle` when the agent stops, so "the row is no longer `running`" *is* "rung 1 worked" |
| 2a | apply `abort_ladder` → `stopped` with the reason | The row first, because the other order leaves a row that says `idle` over a dead container — the one lie a client cannot detect |
| 2b | `stop` the backend | Kills the hung agent, which is what finally makes the turn answer `Aborted` |
| 2c | apply `resume` → `starting`, then the provisioner | "Then immediate resume" (item 16 §2); the four steps are the provisioner's, and that is the path HS-018's endpoint reuses |

`stopped_at` moves with the state inside the same write (ADR-0016), so a forced stop is instantly
eligible for HS-021's idle stop and HS-064's retention without a second statement.

The ladder does **not** call `SessionBackend::resume`. Whether a resumed session runs all four
provisioning steps or only the container's is HS-018's and HS-024's question — the trait's `resume`
exists and has no caller until one of them says what it means. Answering it here would have been a
guess that HS-024 then had to undo.

## Two owners can end one turn, and the table decides

Rung 2 exists precisely because rung 1 did not work, so the turn runner and the ladder both end up
holding the same `running` turn: the ladder applies `running → stopped`, and only then does the stop
it issued make the runner's `start_turn` answer `Aborted`, at which point the runner's own ending is
`abort_completed`, which the table now refuses.

That refusal is **not a bug and must not read like one**: it is the designed outcome of two owners
racing on one row. The runner therefore treats `StateError::Illegal` for the abort ending as a `debug`
line ("the abort ladder ended this turn instead"), and only for that ending. Everywhere else a
refused transition stays what HS-012 made it — an error the caller has to be able to explain. This is
the same trade ADR-0019 accepted for a prompt that loses a race to another prompt: the table is the
authority, and losing a documented race is not a failure.

The reason on rung 1's ending is `abort_completed` — the trigger's own word — because
`session.status` has no trigger field (ADR-0016) and the demo's claim is that "the reason says which
rung fired". A client therefore reads `reason: "abort_completed"` for a clean abort and
`reason: "forced_abort"` for one that had to kill the container. The row's final state clears it: the
resume's transition carries no reason, so a session that came back from a forced abort is `idle` with
`reason: null` and the log is where the history lives.

## `409 session_idle` joins the family

The refusal family ADR-0019 established is `409 session_<state>`, spelled from `Status::as_str`. An
abort needs it for `idle`, which ADR-0019 could not express: `not_idle(Idle)` returned the fixed 500
because a prompt is *accepted* in `idle`. So the constructor became
`not_in_state(status, message)` — total over the six states, one code each, `session_idle` included —
and the **message** is the caller's, because a sentence has to describe its own request: an abort's
refusal says "the session is not running, so it has no turn to abort", a prompt's says "the session is
not idle, so it cannot be prompted". HS-017 needs `session_idle` for steer and gets it for free.

An abort on anything but `running` is refused rather than answered `202` with no work. The spec's
family rule and HS-017's "on `idle` it returns `409 session_idle`" make a silent 202 the odd one out,
and a 202 that does nothing is indistinguishable from a ladder that failed.

## Consequences

- `crates/holosmith/tests/abort.rs` proves: the 202's row is still `running`; rung 1's calls are
  `clear_queue` then `abort` with no `stop`; the turn ends `running → idle` with reason
  `abort_completed`; a hung stub (`ignoring_abort`) reaches `stopped`/`forced_abort` and comes back
  `idle` with the same id and the four provisioning steps; the ladder's reason is the caller's
  (`turn_timeout`); `409 session_starting`, `409 session_idle` twice over, and `404 not_found`; and
  the route is recorded `Mutating`. Every refusal is asserted on status, `code` and `content-type`.
- `src/backend/stub.rs` grows three tests' worth of behaviour: an abort cuts the script short, an
  ignored abort does not, and a stop cuts it either way and leaves no flag behind.
- `scripts/verify.sh` grows two cases (`case_abort`, `case_abort_on_an_idle_session`) — 28 cases —
  and the shipped artifact is where rung 1's `abort_completed` is proved, because rung 2's trigger is
  a constructor argument only a test can set.
- `web/src/main.ts` aborts for real now: a POST with no body and the two guard headers, a notice on
  success and the envelope's code on failure. The badge follows the stream, not the answer.
- `SessionBackend` is now ten methods. `steer` and `resume` still have no caller; HS-017 and HS-018
  are one story away each, which is why they were added with the trait rather than with their callers.
