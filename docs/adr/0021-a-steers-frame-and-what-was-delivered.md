# A steer's frame, and who records that it was delivered

HS-017 is the story that turns the composer's third button into something real. The spec asks for one
endpoint (item 8) and the story adds one requirement the spec does not spell: "The steer is recorded as
an event so the transcript shows what was injected and when." Three things had to be decided, and each
is what HS-032's real agent integration will read.

## The kind is `turn.steer`, with `{"text": …}` and no message id

The event kinds are `<subject>.<verb>`: `turn.start`, `turn.end`, `message.delta`, `message.snapshot`,
`message.end`, `tool.start`, `tool.end`, `session.status`. A steer is something that happened **to a
turn** — the operator's guidance reached a turn that was already running — so the subject is the turn
and the kind is `turn.steer`. It is not `steer.injected`: the glossary lists *inject* under
**Steer**'s avoid-list, and a kind is vocabulary like any other.

The payload carries the text and nothing else. **No message id**, which is a deliberate exception to
ADR-0018's "a message id keys a transcript block":

- ADR-0018's id exists so that a *stream* of frames — deltas, then a snapshot, then the message's end
  — can be folded into one block. A steer is one frame with nothing after it: no second frame would
  ever name the same id, and a snapshot of a steer is not a thing the log can hold.
- The renderer's rule is therefore "a steer frame *is* a block", created when the frame arrives and
  never updated. Replay is unaffected: a reconnecting client applies the log from its own
  `Last-Event-ID`, and a client without one rebuilds the whole page anyway.

The alternative — mint a `steer-<instant>-<n>` id — buys nothing today and invents a third id
namespace (after the session's hex id and the stub's message ids), so it is not done. What it would cost
to add later is worth naming: the log is durable, so a `turn.steer` that grew an id would leave two
shapes of one kind in the same table, and every reader would branch on which it got. That price is paid
by the story that adds the id, not by this one, and the choice there is either a kind of its own
(`turn.steer.end`, say) or keying an unkeyed block by the frame's `seq` — which already travels as the
SSE `id` (ADR-0014) and would generalize ADR-0018's rule rather than except it.

## The agent is asked first; the record follows what was delivered

The handler's order is body → row → state → **agent** → record:

1. `{"guidance": "…"}`, one field, denied unknowns, non-empty after trimming, else `422 bad_body` with
   the steer's own sentence. The field name is the composer's and the glossary's, so the page and the
   API use one word.
2. The row (`not_found`) and its state: only `running` accepts guidance. Everything else is
   `409 session_<state>`, which includes `session_idle` — the code HS-016 added to the family and this
   story is the second caller of.
3. `SessionBackend::steer(&session, &text)`. A refusal is `500 internal`: the client can fix neither a
   backend that does not implement steering nor a turn that ended under the read, and the guidance was
   **not** delivered.
4. `last_active_at` moves (HS-015's acceptance names prompt, settle and steer), and the `turn.steer`
   frame is appended.

The record comes after the agent because the transcript's claim is "this is what the agent was told",
and a frame appended before a failed delivery would be a lie in the log. It is the reverse of the
state-machine rule ("the row moves first, the event follows"), and for the same reason: the event
describes something that already happened. A failed *append* still answers `202` — the guidance did
reach the agent, and a dying writer is not a reason to tell the operator the steer failed (that is the
`StateError::Unreported` trade again).

A steer changes no state. The transition table has no row for it, the handler never asks for one, and
`last_active_at` is a repository touch rather than a status write, so `no_status_write_outside_the_one_function`
still holds over the new code.

## The UI sends it, and says the one refusal worth a sentence

`Steer` posts `{"guidance": …}` with the two guard headers, exactly as the prompt and the abort do, and
the 202 means the running turn took it: the composer empties itself and the block appears where it
landed in the turn, drawn as `article.steer` with the operator's text.

The one refusal with a different action behind it is `409 session_idle`: the turn ended while the
operator was typing, so the guidance is a prompt. The page says so ("This turn has ended; send it as a
prompt instead") and leaves the text in the box — and the Prompt button is already the visible action,
because that is what `affordances("idle")` says. No second control, no automatic resend: a page that
silently changed the request the operator made would be worse than one that says what happened.

## Consequences

- `crates/holosmith/tests/steer.rs` proves: the 202's row is still `running`; the stub recorded the
  steer with the same text; the log holds a `turn.steer` whose `text` is what was sent; the turn still
  settles on its own afterwards; `last_active_at` moved; `409 session_idle` on an idle session,
  `409 session_stopped` on a stopped one (driven through the table, because HS-018's endpoint does not
  exist yet), `404` for an unknown id, nine `422 bad_body` shapes with no steer reaching the agent, and
  the route recorded `Mutating`.
- `web/test/transcript.test.ts` grows two tests: a `turn.steer` frame parses to one steer change, three
  steers are three blocks (no folding, no message id), a steer is not in `order()`, and a frame with no
  string `text` is dropped rather than drawn empty.
- `scripts/verify.sh` grows two cases: `case_steer` (the 202, the frame on the stream, then the row
  polled until it says `idle` — 30 cases) and `case_steer_on_an_idle_session` (the `409`).
- `SessionBackend::steer` has its first production caller. `resume` is now the only trait method
  without one, which is HS-018's.
- The steer handler holds no lock across an `.await`: one synchronous read for the row, the backend
  call, then one touch and one append. The touch and the append are logged rather than fatal, for the
  reason the ladder's are.
