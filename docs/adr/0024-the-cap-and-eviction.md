# The cap stops a sleeping session rather than refusing the create

HS-020 is the story that makes the cap a policy instead of a wall: a create on a full machine stops the
least recently active idle session and takes its slot. Four things had to be decided, and the third is
the one this story really is.

## The eviction is the create's, and the create cannot do it inside its own lock scope

The create has one lock scope — lookup, cap count, insert — precisely so two creates cannot both see the
last slot (ADR-0012). An eviction cannot run there: it moves a row, and every state change goes through
`Transitioner::apply`, which takes the database lock itself (ADR-0016). Calling it from inside the scope
would deadlock, and writing the status directly would be the second way to change a state that the
whole architecture exists to prevent.

So the create became: try the one scope; if it answers "full", evict; try again. `Slot` is that one
scope's three answers — `Inserted`, `Existing` (the idempotency key already named a session) and `Full` —
because a `bool` and a sentinel cannot tell the handler which of the three next steps to take.

**The loop is bounded, and it terminates by construction**: each successful eviction turns an `idle`
session into a `stopped` one, so the live count strictly falls, and `max_live_sessions` has a floor of
one. The bound (`max_live_sessions + 1` attempts) exists only for two creates evicting in each other's
wake, where the honest answer is `409 cap_reached` rather than a wait with no end.

**The retry is never evicted for.** The idempotency lookup runs before the cap count, so a create whose
first attempt filled the machine still gets its own session back — and, since this story, does not stop
anybody's session on the way to finding out it already had one.

## Only `idle` is evictable, and the victim is chosen by `last_active_at`

The story's third bullet is the rule: a `starting` or `running` session is live and **not** evictable, so
a create that finds nothing idle answers `409 cap_reached` with the create's own sentence ("…and none of
them is idle to stop"), which tells the operator to wait for the turn rather than to tap again.

`SessionRepository::oldest_idle` is one statement: `WHERE status = 'idle' ORDER BY last_active_at ASC,
id ASC LIMIT 1`. One row, the same `last_active_at` the list orders by, and `id ASC` after it so two
sessions touched in the same second still have one answer. `stopped`, `error` and `deleting` hold no
slot, so they are not candidates; a session that stopped itself is not stopped twice.

## The row moves before the container, and the container is best effort

ADR-0020's order, for its reason: a row that says `idle` over a dead container is the one lie a client
cannot detect. Here the direction is even cheaper — the row becomes `stopped` and the slot is free
whatever the container does — so a failed `backend.stop` is logged and the create proceeds. The operator
can still delete a session whose container outlived it, and HS-024's Docker backend is where "stop
actually stops" is proved.

`EVICTED` is the third reason word (`forced_abort`, `deleted`, `evicted`), and it is a *reason* rather
than a state because a stopped session is a stopped session: what differs is why, and only the reason
carries that.

## The page says the cap lost nothing, and says it in one word

Spec item 9 is emphatic — "Nothing is deleted by the cap" — but a badge reading `stopped · evicted` reads
like a loss. So `web/src/transcript.ts` gained `stoppedNotice(reason)`: `evicted` becomes "stopped to
make room — nothing was deleted, resume it to continue", and **every other reason returns `null`**.

One word wide, deliberately. A forced abort did not preserve work in the same sense, and a delete's own
reason must not be softened by a sentence written for the cap; inventing reassurance for either would be
a claim about work nobody saved. The function lives in `transcript.ts` rather than in `main.ts` because
that file's rules are tested without a browser (`npm test`), and the list row uses it in the span it
already reserves for a stopped session's detail (HS-064's days-remaining line takes the same one).

## What this story left alone

**The resume keeps its `409 cap_reached`.** Spec item 8 says a resume on a full machine "evicts the least
recently active idle session first, **or** returns 409", and ADR-0022 chose the refusal to keep a refused
resume from stranding a row in `starting`. That is still true, and giving the resume the eviction would
be a second policy change with no bullet behind it — so a resume on a full machine still refuses, and a
create now evicts. The asymmetry is deliberate and recorded rather than accidental.

**Nothing evicts on a timer.** An idle session is only stopped when a create needs its slot; HS-021's
idle stop is the policy that stops one because it has been quiet, and HS-044's drain the policy that
stops them all.

## Consequences

- `crates/holosmith/tests/eviction.rs` proves: the round trip (the create is accepted, the victim is
  `stopped` with `evicted` and its `stopped_at` moved, the live count is one, the victim's worktree path
  is untouched, the backend was asked to `Stop` and never to `Delete` or `Snapshot`, the transcript is
  the old log with the eviction appended, and the row still answers `200 stopped`); the victim is the
  least recently active, proved in the direction that rules out "oldest created" and "lowest id" by
  ageing the *newer* session and watching it die; the two refusals (`starting` and `running`, with the
  sentence that says nothing was idle and a backend that was never called); and that `error` takes no
  slot.
- The bits the story did not build are now evidence rather than assumption: the cap counts the three
  live states (`live_count`, HS-008) and `max_live_sessions` is validated `≥ 1` by the config's semantic
  pass — both hold, and the tests above depend on the first and run on the second.
- `web/test/transcript.test.ts` grows two cases for `stoppedNotice`, and `npm test` is 20.
- `scripts/verify.sh` grows `case_create_at_cap_evicts` (a self-contained pair, run last, asserting the
  victim's row, its replayed transcript and the eviction's own event) and makes `case_create_cap_reached`
  assert its own precondition, because since this story a create at the cap is only refused when the live
  session is not idle.
