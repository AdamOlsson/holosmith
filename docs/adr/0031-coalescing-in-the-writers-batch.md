# Coalescing happens in the writer's batch, and the writer parses one more payload

HS-059 is spec item 15's first sentence: "inside each 25 ms commit, consecutive deltas of the same kind and
message become one row with one seq." That names both the place and the unit — the commit and its batch —
and three consequences follow: who merges, what must match, and what happens to a payload the writer does
not understand.

## The writer merges, not the producer and not the repository

The batch exists only inside `events::writer`: `collect` gathers what arrived inside the window, `commit`
turns it into rows in one transaction, and the broadcast follows. ADR-0013 made that the one place a seq is
assigned, and a merge is exactly "assign one seq to what were going to be several events". A producer that
merged would be deciding on its own how the log is written (and the stub, the pi mapping and a hub's
forwarded frames would each need the same rule), and a repository that merged would be rewriting rows it
was handed.

So `coalesce(batch) -> Vec<NewEvent>` runs between `collect` and `commit`, and it is a pure function over
values: one pass, no I/O, no clock. The writer's own loop is unchanged.

## Four boundaries, and a frame is one of them

An event merges into the row before it only when **all** of these match: the session (one batch can carry
several sessions' events), the kind (`message.delta` and nothing else), the delta's **message id** (a
transcript block is keyed by it, ADR-0018), and the delta's own kind (`text` or `thinking`, two nodes in the
same block).

The fifth boundary is adjacency itself: a `tool.start`, a `message.end` or anything else between two deltas
ends the run. The transcript depends on the order of a tool call against the text around it, so "consecutive"
means consecutive in the batch and not "the same message later in the batch". A consequence worth stating
because it surprised the test that first asserted otherwise: after a *different* commit, two rows of one
message and kind may still sit next to each other in the log. Coalescing is a rule about one batch; the log's
adjacent rows are not.

The merged row keeps the **first** event's `created_at` — a row stands for a span, and the span began there —
and its payload is re-serialised from a struct whose field order is the contract's, so a replay cannot tell a
merged row from one a producer wrote.

## The writer reads a second payload, on purpose

ADR-0027 recorded that the **events repository** parses exactly one kind's payload (`message.end`'s `usage`),
so the tally could commit with the row. This is a second parse, in a different place and for a different
reason: the writer is looking at the event as a value on its way to being a row, and "same message and kind"
is unanswerable without reading the payload. The repository's invariant is untouched — it still parses
exactly one kind.

A payload that is not the pinned shape is **never merged**: it is written exactly as the producer sent it,
and it ends the run it sits in. "The pinned shape" is exact — the three keys and nothing else — because
merging a payload with a fourth key would drop that key, which is the same invention in the other direction.
The writer is not a validator, and rewriting a payload it does not understand is not its job.

A run's text is accumulated **outside** the rows and serialised **once**, when the run ends, and a run of
one is never re-serialised at all. That is the same argument ADR-0018 makes against re-reading a message to
append a delta: a merge that re-parsed and re-wrote the accumulated payload per delta would cost the square
of a run's length, on the one thread every session's log shares.

## What the gate can and cannot show

The shipped artifact's stub paces one frame per 200 ms — eight commit windows apart — so a gate case cannot
produce a burst, and a case that claimed to prove the merge there would be proving nothing. The merge is
proved where a burst can be made: `tests/turns.rs` (the script's seven deltas become three rows at a pace
below the window) and `tests/events.rs` (5,000 deltas become three rows, with the text preserved). The gate
case (`case_event_coalescing`) asserts the contract a merge must keep — never more rows than the script's
frames, one message id, no lost or reordered text — and says in its own comment which half it is.

The rendering side of "the transcript is identical" is two node tests: three deltas and one merged delta
fold to the same block and the same text, and the coalescer's exact payload bytes are fed through the page's
own frame parser — otherwise each half could be right and the pair wrong.

The live-path test gives the writer a **two-second** commit interval, which is longer than a whole
zero-paced turn, so "inside one commit" is a fact of that test rather than a hope about the host's speed;
that is what lets it assert a row count and adjacency. At the production 25 ms the same turn can straddle a
commit, and then two adjacent rows of one message and kind are correct.
