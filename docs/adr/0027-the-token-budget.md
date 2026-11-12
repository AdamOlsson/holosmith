# The token budget counts a column the writer adds to, and the runner is what trips it

This is spec item 13's second row: at `max_session_tokens` the turn is aborted through the shared ladder
with reason `budget_exceeded`. Two questions had to be answered before any code — **where the tally is
written**, and **who compares it with the ceiling** — and a third, smaller one about a turn that settles
over its ceiling before anybody notices.

## The tally is the event writer's, in the transaction that assigns the seq

`usage_in` and `usage_out` have existed since migration 0002 and were always 0, because nothing persisted
usage. The frame that carries it is `message.end`'s `{"usage": {"in", "out"}}` — emitted by the *backend*,
from inside the turn, which the runner never sees: `start_turn` is handed an `EventWriter` and the frames
go straight into it (ADR-0019). So the choice is between a second reader of the log and the writer itself.

`EventRepository::insert_batch` already borrows the sessions repository through its own transaction, to
bump `last_seq`. The same borrow now adds `SessionRepository::add_usage(id, in, out)` for a `message.end`
that carries counts, so the event's row and the session's tally commit together and the tally is durable
before the frame is broadcast. The price is stated rather than hidden: **the events repository looks inside
one kind's payload**; every other kind is opaque to it, and `usage()` is the only function in the file that
parses one. The alternatives were both worse. A broadcast subscriber in the runner has lag semantics (a
dropped frame is a lost count, silently) and would be a second parser of the same JSON. A second write after
the commit would be a second writer, which is the thing ADR-0013 exists to prevent.

**Amended by ADR-0032:** "exactly one kind" is no longer true. Compaction has to read a delta's message id
and fold its text, and to read the id a `message.end` names, so the repository now parses three payload
shapes and writes one. The *argument* above is unchanged — the repository is still the one place a row and
the thing derived from it commit together — but the count is three, not one.

The column is added to, never set: a turn reports what *it* spent, and the session's spend is what all of
its turns spent. `SessionRepository::spend` sums the two columns in SQL, so the ceiling is one comparison
against one value, and no caller has to remember to add them. A session that is resumed keeps its tally —
the spec says "per session", and a resume continues the same work.

## The ceiling is the turn runner's, on a poll, and a climb is spawned

`Turn` gains `budget: Option<u64>` and its `select!` a third arm: every hundred milliseconds it reads the
row and compares. At or over the ceiling it climbs the ladder with `BUDGET_EXCEEDED`, from a task of its
own, and keeps awaiting the turn — the same shape, and the same three reasons, as ADR-0026's deadline. A
policy that must *record* a reason owns the ending, because only the runner writes one; a `select!` branch
that awaited a climb can be dropped mid-climb; and rung 1 working *is* the turn future resolving.

**Whichever policy fires first owns the reason.** Both arms are guarded on a single `fired`, so a second
climb is never spawned after the first: two climbs would ask a second abort of a turn that is already
ending, and the row has one `status_reason` anyway. A turn that trips the budget *and* its deadline records
the one that fired first.

`None` builds no poll at all, the reading and gave their own keys: a key the operator did not
set should cost nothing. `0` is not this story's to decide — the config validator already refuses it with
`must be at least 1, or unset`.

## A turn whose last frame is its `message.end` is labelled, not aborted

The poll runs every hundred milliseconds; the stub's turn emits `message.end` one frame before it ends, and
a real agent's message usually ends the turn too. So a turn can *settle* between two polls with the tally
already over the ceiling. Nothing is left to abort, and the honest answer is the one already gave for
a turn that finished after its deadline: record the reason on the ending it wrote. `Turn::finished` therefore
gives the budget one last look in its `Settled` arm only — the row reads `idle · budget_exceeded`, the
`session.status` frame carries it, and the *next* turn trips on its first poll, before it does any work. An
abort, or an error, keeps its own reason: an operator's abort is not the budget's to relabel.

Because the tally is committed by the writer on its own tick, the last look is not a barrier: a writer that
falls behind can still answer from the row as it was one frame ago, and the label is then one turn late
rather than wrong — the next turn's first poll catches it. Looking twice, one tick apart, is enough for a
writer that commits on schedule, and the delay is paid on every ordinary turn of a budgeted session (25 ms
against a turn), while a session with no ceiling — the default — pays none of it. The interval is derived
from `WriterOptions::default().commit_interval` rather than restated, so a construction site with a
different tick moves the wait with it.

The alternative — making the trip deterministic by checking only *before* a turn — would let a session run an
unbounded number of turns past its ceiling in a single conversation. The alternative — poll faster than the
frame — only shrinks the window, and pays for it with reads nobody needs.

**The ceiling is reached, not overshot.** `spend >= max_session_tokens` is spec item 13's "at": a session
that has spent exactly its ceiling is over it, and `>` would leave the operator one token above the line for
one more whole turn.

## A backend that reports no usage cannot trip anything, and the screen says so

Spec item 13's contingency: if the backend reports no usage, the budget has nothing to count. That needs no
flag and no fallback — the counts stay 0 and `0 >= max_session_tokens` is false for every ceiling the
validator accepts — but the operator has to be told, or a ceiling that is silently off looks like one that
is on. The precise claim is that **the budget counts what was reported**: a backend that reports usage for
one message and not the next under-counts, and no rule here can detect it, because a message with no counts
is indistinguishable from a free one at the session's row. Per-message honesty is the backend's to keep
and the sentence below is what says the count is
not to be trusted as a total. The screen's sentence is a rule about the *stream*, not about the row's total:
`budgetNotice({settled, usage})` in `web/src/transcript.ts` says nothing until a turn has settled, and says
`no usage reported — the token budget has nothing to count` when one settles without a `message.end` that
carried counts. A row whose two columns are 0 cannot tell "no turn has run yet" from "the backend reports
nothing"; a settled-turn boundary and a usage frame can, and both are in the log, which the page replays on
every load.

Both ends of the frame agree on what a count is: the writer takes two non-negative integers
(`serde_json`'s `as_u64`) and the renderer refuses anything else (`Number.isInteger` and `>= 0` in
`transcript.ts`), so a fractional or negative `usage` is neither counted nor drawn. A screen that rendered
`in -5` would be showing a cost the budget never added.

The stub gets `omitting_usage()`, next to `ignoring_abort` and `failing_snapshot`: a constructor argument
and never a config key, because nothing an operator runs should be able to blind the budget.

## What this story left alone

**Not the totals on the screen.** draws `usage_in · usage_out` on the session screen; this story only
makes the columns non-zero, which is what turns them from decoration into a fact.

**No new config key, no schema change, no migration, no new endpoint.** `max_session_tokens` was in the
config table from ADR-0006 and its golden output does not move; the columns are migration 0002's; the row's
JSON already carries both.

**No sentence for `budget_exceeded` in `stoppedNotice`.** Rung 1 normally ends the turn, so the row is `idle`
with the reason and no reassurance is owed; the stopped row's two sentences stay the two ADR-0024 gave it.

## Consequences

- `crates/holosmith/tests/token_budget.rs` proves, in eight tests: a session over its ceiling has its turn
  ended with `budget_exceeded`, by the ladder (`clear_queue` + `abort`), the row `idle` and the tally not
  charged for a message the aborted turn never sent; a turn that settles over its ceiling is labelled
  without a climb; the first policy to fire is the only one that climbs (one `abort`, not two, with a
  deadline the turn would have outlived); two turns that both report usage add up (2400 / 680 — the test a
  `SET` in `add_usage` fails, and the only one that does); a session that has spent exactly its ceiling is
  over it (the test a `>` in `over_budget` fails); an unset ceiling never ends a turn while the tally is
  still persisted; a `message.end` with no usage neither resets nor doubles the tally; and a backend that
  reports no usage cannot trip a ceiling of 1.
- Four mutations were run and each turned the suite red where it names: `add_usage`'s `SET` for `ADD`
  (`two_turns_add_up`), `spend >= ceiling` to `>` (`a_session_that_has_spent_exactly_its_ceiling_is_over_it`),
  the poll's climb deleted (`a_turn_that_spends_the_budget_ends_with_the_reason` and
  `the_first_policy_to_fire_is_the_only_one_that_climbs`), the budget arm's `fired` guard deleted
  (`the_first_policy_to_fire_is_the_only_one_that_climbs`), and the settle's last look deleted
  (`a_turn_that_settles_over_its_ceiling_is_labelled_without_a_climb`).
- `web/test/transcript.test.ts` pins the sentence: silent before a turn settles, silent when usage arrived,
  the sentence otherwise, and not a word shared with `stoppedNotice`'s two.
- The gate adds `case_token_budget`, the only case that boots the artifact twice: the ceiling cannot be in
  the shared config, because every prompt above it would then end `budget_exceeded`. The firing is proved
  from the shipped artifact — `max_session_tokens = 1000` is under the stub's 1540 per turn — and which of
  the two endings records the reason is left to the poll, so the case asserts the reason and not the path.
