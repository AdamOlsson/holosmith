# The transition table as data, one function that applies it, and how "impossible" is enforced

 gave the product its first state changes: the provisioner wrote `starting → idle` and
`starting → error` straight through the repository's status write, with the event payload
`{step, status, reason}`. Twelve more stories will change a session's state — prompt, abort, evict,
idle stop, drain, resume, delete, crash recovery — and every one of them would otherwise decide for
itself which moves are legal. This is the story that moves that decision into one table before
there is a second caller to disagree with the first.

The spec settles the states and their triggers (item 8) but not how they are represented, what a
refusal looks like, or how "direct writes to `status` are impossible outside it" is enforced. This
records those choices.

## The table is `(from, trigger) -> to`, and one row is one state change

`sessions::state::TABLE` is a `&'static [Transition]`, each row a `from`, a `trigger` and a `to`.
`FromStates` is `None` (no row exists yet), `OneOf(&[Status])`, or `Any` — because the spec's table
says "any" for delete and lists three states for an unexpected container exit. A `&[Status]` slice
rather than a bitflag or a builder: the table is data a reader compares against the spec's markdown
row by row, and a test does exactly that.

Two rows need saying out loud, and both say "the row is one change":

- `running` + `abort_ladder` ends in `stopped`. The spec's cell continues "then immediate resume" —
  that is a *second* row (`stopped` + `resume`), which chains. Two rows, two events, two
  writes: a single row with a sequence in it would be a second mechanism for the same thing.
- `any` + `delete` reaches `deleting`. "then removed" is the row's removal, which is the flow
  and not a state.

Recovery is the one row whose `from` needed interpreting. The spec's cell says "server crash", which
is not a state, so the row admits the states that were **live** when the process died —
`starting`, `idle`, `running` — and not the two that need no recovery. That matters: a row admitted
from `stopped` would go through the same write that clears `stopped_at`, restarting item 15's
7-day retention clock and counting a stopped session live again. A row caught mid-`deleting` is
the decision, and until then the table refuses it rather than restarting a session somebody
asked to delete.

The table reaches exactly the spec's six states. `ended` is not one of them and cannot be spelled:
it is not a `Status` variant, and `Status`'s parse refuses it. `interrupted` is a *reason*
— the one the crash-recovery row carries — and `tests/state_machine.rs` proves that: recovery takes
a `running` session to `starting` with `status_reason = "interrupted"`.

## One function, and the event is part of it

`Transitioner::apply(session_id, trigger, reason)` is the only production caller of the repository's
status *change*. One clarification the wording above needs: the create's `INSERT` also writes the
column — but it writes the table's own `none` row (`initial_status()`), not a value a story chose,
and `sessions/repository.rs` is exempt from the guard as a whole file because the repository owns
every statement against its table. So the rule is: nothing changes an existing session's state
outside `apply`. `apply` reads the current state, looks the trigger up, and either:

- **refuses** — logs `from`, `trigger` and the session at `error`, returns
  `StateError::Illegal { from, trigger }`, and writes nothing at all; or
- **applies** — writes the new state with its reason and `stopped_at` moved in the same statement,
  emits `session.status` **after** the write, logs `from`/`to`/`trigger` at `info`, and returns the
  new state.

The `Transitioner` carries both handles (the database and the event writer) so a caller cannot have
one without the other and forget the event.

**Ordering, recorded now because twelve stories will call this:** the write happens inside the
mutex and the event is appended after it is released, so two concurrent `apply` calls on one session
could enqueue their events in the opposite order to their writes, and the writer's seq assignment
follows the queue (ADR-0013). Nothing can reach that today — one provisioner task per session calls
`apply` sequentially, and no other caller exists — but per-session sequencing of concurrent state
changes is not this story's, and a story that lets two tasks change one session at once owns it. `AppState::transitioner()` wires it from the state a
handler already has.

**On the acceptance's "logged with from/to/trigger":** a request is `(from, trigger)` — the caller
never names a target, because the table is what decides it. So a *refused* move has no `to` to log;
its line carries `from` and `trigger`, and the refusal's message is the same two words. An applied
move logs all three. Inventing a target the caller did not ask for, to satisfy the wording, would be
worse than saying this plainly.

`stopped_at` is set when the state becomes `stopped` and cleared when it leaves — inside
`set_status`'s one `UPDATE`, so there is no window in which a `stopped` row has no `stopped_at`, and
no story can set the state without moving the column with it. That is item 15's "cleared on
resume", and it is why the move is one function rather than a helper beside each caller.

## The payload gains `from`, `to` and `step` in one shape

```text
{"from":"starting","to":"starting","step":"worktree","reason":null}   a provisioning step starting
{"from":"starting","to":"idle","step":null,"reason":null}             the transition it led to
```

Four keys, always present, explicit `null`s — the rule the session row follows. the payload was
`{step, status, reason}`; `status` becomes `to` and `from` joins it, so an event names the change it
reports rather than only where it ended. A provisioning step report is a payload with `step` set and
`from == to`: a step beginning changes no state, which is also why it is *not* a table row and not a
call to `apply` — the provisioner emits it directly and writes nothing. The two real transitions the
provisioner makes are `apply(PiReady)` and `apply(ProvisioningFailed)`, so the direct status
writes are gone.

A transition's payload carries `step: null`. A failure therefore names its failing step through the
step report that preceded it, which is always the same step, because a step is announced before it
runs.

## "Impossible outside it" is a source-level rule, not a type

The repository's `set_status` is still `pub`, and that is deliberate: `tests/session_repository.rs`
is the proof that the repository can update a state at all, and it lives outside the crate, so
a `pub(crate)` write would delete real coverage to buy a compile error.

The rule is enforced where this repository already enforces its other seams — by a test that walks
`src/**` and fails if any file other than `sessions/repository.rs` (the definition) and
`sessions/state.rs` (the one caller) so much as names `set_status`. The walk is
`crate::sql::guard::sources_under`, extracted for this second caller rather than copied. It is the
same mechanism as "the repository owns every statement against its table" and the `.layer(`
single-assembly check, and it has the same property: it fails on a new offender even when no test
exercises that offender's path. `the_guard_fires_on_a_planted_write` proves the matcher notices,
because a rule that passes over today's tree is a rule that might be passing for the wrong reason.

The crate boundary is the honest limit: production code is `src/`, and an integration test that
writes a state directly is testing the repository, not the product.

## Consequences

- `scripts/verify.sh` grows `case_state_transition_payload`: on the shipped artifact, the four step
  reports claim the unchanged state and the fifth is exactly
  `{"from":"starting","to":"idle","step":null,"reason":null}`. 20 cases, 60 assertions.
- `tests/sessions_api.rs`'s stopped-session test no longer writes `stopped` directly — it waits for
  the provisioner's `idle` and then applies the table's `evict` row, which is what the product does.

- `Trigger`'s variants are `pub`, so the twelve that no story calls yet are the API the next twelve
  stories will use rather than dead code. The transitioner reaches `apply` through `AppState`'s
  transitioner, and none of them needs to touch the table to add a state — only a story that adds a
  *trigger* does, which is spec item 8's business.
- (delete), (evict), (idle stop), (abort ladder), (recovery) and
   (drain) all have their row already. The one thing they must not do is write `status`.
