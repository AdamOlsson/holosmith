# Compaction is a per-session pass inside the database lock

HS-060 is spec item 15's second sentence: for messages completed more than 24 hours ago, a compaction job
replaces their delta rows with one `message.snapshot` taking the highest seq of the removed rows. The frame,
the renderer and the seq counter already existed — ADR-0018 pinned the snapshot's payload and the page
already replaces on one, and ADR-0031's neighbour rule says the counter is `sessions.last_seq` precisely
because something was going to delete rows. What the story had to decide is who runs the job, what makes a
message eligible, what the write must be atomic with, and what a client sees.

## The window is the spec's number, not a key

Spec item 15 says 24 hours, and ADR-0006 records that the spec's config list is exhaustive — so a
`snapshot_after_hours` key would be an invented setting, not a detail. `compactor::SNAPSHOT_AFTER` is the
one spelling of "old", and `Compactor::with_window` exists so a test can choose its own: a test writes its
message with an instant two hours old (an event's `created_at` is the **producer's**, so no aging hook is
needed) and passes a one-hour window.

That is also why the gate case is the only one that touches a database: the shipped stub writes `now`, so
`case_compaction` stops the artifact, ages the log by 25 hours with `python3`'s `sqlite3`, and starts it
again. The restart is not a workaround, it is the story's own demo.

## A message is eligible when it is complete, still has deltas, and has no snapshot

`message.end` is the only durable instant that says a message finished, so it is what the window measures —
and a message still being streamed has no `message.end`, which is one of the two reasons a turn in flight
cannot have its text folded away underneath it. "Still has deltas" and "has no snapshot" are one condition
seen from both sides: `message.end` rows are never removed, so without them a pass would re-read a folded
message for ever, and a bounded pass would then never advance past the messages it had already folded.

The fold is strict, and it reuses the writer's parser rather than writing a second one. A `message.delta`
row whose payload is not the pinned three keys names no message id, so it cannot be attributed — and folding
the rows that *can* be attributed would delete text this pass cannot account for. Such a row therefore stops
the whole session for that pass: nothing is written, and the pass logs one `warn` naming the session and the
count. Compaction's contract is "without losing content", and a payload the contract does not describe is the
producer's bug to fix, not a reason to guess. The cost is stated rather than hidden: that session stops being
compacted until the producer is fixed, which is why the line is a `warn` and not a `debug`.

Reusing `parse_delta` has a documentation consequence. ADR-0027 and ADR-0031 both record that "the events
repository parses exactly one kind's payload", and that is no longer true: `compact` reads a delta's pinned
shape, reads the `message_id` a `message.end` or a `message.snapshot` names, and *writes* a snapshot payload.
Three parses and one write, all in the one file that owns the table — the argument ADR-0027 makes (the row
and the thing derived from it commit together) is unchanged, but the count it states is superseded.

## The write is one transaction per session, inside the lock the transitioner takes

The story's "never while a turn is running for that session" is answered by mutual exclusion rather than by
a second lock or a re-check protocol. `Transitioner::apply` takes `Arc<Mutex<Database>>` for every state
change, and so does the per-session pass: a turn therefore starts either **before** the pass — and the row
says `running`, which the pass re-reads and skips — or **after** it, when the pass is over. There is no
window between the read and the write for a prompt to slip into.

The re-read is the authority and the candidate filter is only a guess about a moment that has passed, which
is why `Compactor::fold` is public: a test has to be able to ask the per-session step directly, because a
test that only ever calls `sweep` cannot tell the two apart.

One transaction covers a whole session's pass — the story's "batched" — because a client must not be able to
read a log with some of a message's deltas gone and its snapshot not yet written. `MESSAGES_PER_PASS` bounds
how many messages one pass folds so the mutex is not held for an unbounded time, and the bound is applied
**after** the already-folded filter, in Rust: a `LIMIT` in the query would keep taking the same oldest
messages and a limited pass would never advance.

Bounding the number of messages is not the same as bounding the *work*, and the difference matters because
the mutex is the one the writer and every handler need. So the delta read is bounded too, by the seq of the
newest candidate's `message.end`: a message's deltas all precede its own end, so that seq is the furthest row
any fold can reach, and without it a pass would read and parse every delta the session had ever held — on
HS-061's 200k-event session, hundreds of milliseconds every minute. The bound is on the *scan*, not on the
deletes: a delta inside the region that belongs to a message this pass is not folding is filtered out by
message id, and the deletes are exactly the rows the fold read.

## The snapshot reuses a seq, and nothing is broadcast

The snapshot takes the highest seq among the rows it removes, which is the spec's own rule and the one place
a seq may be reused: every other removed seq becomes a gap. `sessions.last_seq` is neither read nor bumped —
it is the counter that must never rewind, and seq assignment still derives from it alone (ADR-0013).

Compaction does not broadcast. A client already connected has the deltas and its transcript is unchanged; a
client that reconnects reads the snapshot from its replay. Only the writer assigns a seq and broadcasts after
its own commit, and compaction is not the writer — giving it the broadcast sender would be a second place a
client's seq could come from.

## When it runs

A pass at startup, then a fixed 60 s tick — `src/sweeper.rs`'s shape (ADR-0025), for the same reason: how
often a running process looks at a column is not a machine setting. The immediate first pass is
load-bearing rather than tidy, because it is what makes a restart fold a day-old log at once instead of a
minute later, and it is what makes the gate case affordable.

## What the gate shows and what it does not

`case_compaction` shows the rows and the wire: one `message.snapshot`, zero `message.delta` rows, the whole
text in the frame, and `message.end` still there. The rendered-transcript half is `tests/compaction.rs`,
which folds a replay through the boundary the way `web/src/transcript.ts` does and asserts an identical
transcript — and asserts the log actually shrank, because otherwise the equality would hold over an
unchanged log.

What no test can show is that compaction is ever *needed* on this machine: the gate's stub writes one frame
per 200 ms, so a turn's deltas are seven rows and the fold is seven rows into one.
