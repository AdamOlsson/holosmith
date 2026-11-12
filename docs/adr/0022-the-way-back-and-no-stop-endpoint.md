# The way back, and why there is no stop endpoint

This is the story that makes `stopped` survivable: a session whose container is gone keeps its id, its
events and its worktree, and `POST /api/sessions/{id}/resume` brings it back. Three things had to be
decided, and the first is a decision *not* to build something, which is the one most likely to be
re-litigated later.

## Nothing in the API stops a session, and that is deliberate

The spec's API table (item 8) lists `resume`, `delete`, `merge`, `publish` and the read endpoints. It
lists no stop. The transition table reaches `stopped` from `idle` by `evict`, `idle_stop` and `drain`,
and from a killed `running` turn by `abort_ladder` — three *policies* and one *escalation*, none of them
an operator action.

So builds the way back and no way in. Adding `POST /api/sessions/{id}/stop` would have been a
second vocabulary for a decision the later stories own (the eviction, the idle stop, that story's
drain), and it would have made "stop" look like something an operator is expected to do. The
consequence is worth stating plainly: **until or lands, no client can put a session into
`stopped`.** That is why this story's own tests and demo drive the one state function to stop a
session, exactly as the forced rung is driven, and why `scripts/verify.sh` proves the two refusals
and leaves the round trip to `tests/resume.rs`.

`SessionBackend::resume` is still uncalled, for a related reason: a session comes back by being
provisioned (`stopped → starting` is the table's `resume` row, then the four steps), and whether a
resumed session re-runs every step or only the container's is the question about a real container.
Calling `resume` here would have guessed the answer. ADR-0020 deferred it; this ADR keeps the deferral
and says who lifts it.

## The cap is checked before the transition, unlike the create's

The create does its lookup, cap count and insert inside one lock scope, so two creates cannot both take
the last slot. A resume cannot: it is `live_count` and then a transition, two statements in two scopes.
The order that matters is not atomicity but **which side of the transition the refusal falls on**:

- Refusing *after* the transition leaves a session `starting` with nothing provisioning it — a state
  with no way out until the idle stop, and precisely the kind of stranded row this phase has been
  closing.
- Refusing *before* it touches nothing, and the client sees the code it can act on.

So the count is read first and `409 cap_reached` is answered before `apply`. The window between the two
statements can let two resumes overrun the cap by one; the eviction makes that uninteresting, and
a stranded `starting` row is worse than a fourth live session for a moment.

The story's bullet allows either branch — "evicts the least recently active idle session first, **or**
returns 409" — and eviction is that story's, so this story takes the 409. What the operator sees is the
create's own `cap_reached`, so a client has one code to learn for "this machine is full".

## The page resumes once and retries once

The composer's actions are the three the spec's rows name, and now a fourth: `Resume` appears in exactly
the two states the table's `resume` rows accept, `stopped` and `error`, and nowhere else. That is
`affordances`, and it is tested as a matrix because it is the one place a UI can quietly offer a request
the API will refuse.

The retry rule is one helper: the prompt and the steer are POSTs through the same function, and a
refusal with `session_stopped` or `session_error` — the two codes a resume can fix — resumes, waits for
the row to say `idle`, and sends the same request **once** more. **Abort is the exception and passes
`mayResume: false`**: a resume ends with the row `idle`, and abort needs it `running`, so a retried
abort could only ever answer `session_idle` — after paying for a provisioning run the operator did not
ask for. The spec's "the UI resumes and retries" is written for the requests a resume can make legal,
and an abort is not one of them. Four details are deliberate:

- **Once.** A resume that is refused again, or a second refusal, is reported to the operator with its
  code. A page that retried until it worked would turn a broken machine into a spinner.
- **Wait for `idle`, with a bound.** The resume's 202 promises `starting`; a prompt or steer needs the
  provisioning finished. The page polls the row for up to ten seconds and then says the session did not
  come back, rather than sending a request into a state that will refuse it.
- **The badge follows the row, not the retry.** The 202s carry the row, so `showState` is called with
  what the server actually said, and the stream continues to be the authority afterwards.

## Two deferrals this story cannot close, recorded so they are not rediscovered

**Spec item 8's `error` row is conditional** — `| error | resume (clone present) | starting |` — and the
condition is not implemented here: the table's row is unconditional and the handler checks only the
status. Whether a session that died before its worktree was cloned can be resumed at all is a question
about what a stopped session still owns, which is the (the first story with a real container and a
real worktree). This story accepts an `error` resume and lets the provisioner say what is missing.

**A resumed session re-runs all four provisioning steps, including `worktree`, and the provisioner's
failure path undoes what completed.** For a session whose worktree the stop deliberately preserved
("history, worktree, events and images are kept", spec item 9), a later step failing could therefore
remove the very thing the stop kept. Unreachable today — the Docker backend refuses every step, so the
undo is a no-op recording — but it is a real risk the moment fills the trait in, and it belongs
in the brief rather than in this story's code.

## Consequences

- `crates/holosmith/tests/resume.rs` proves: the 202 with the row `starting` and then `idle`; the same
  id, a cleared `stopped_at` and an unchanged `worktree`; the pre-stop event log is a **prefix** of the
  log after the resume, with `stopped → starting` and `starting → idle` appended; `error` is resumable
  too (the table's second row); `409 session_idle`, `409 session_running` and `409 session_starting`;
  `404` for an unknown id; `409 cap_reached` at the cap with the row left `stopped`; the three
  `409 session_stopped` refusals the page retries; and the route recorded `Mutating`.
- `web/test/transcript.test.ts` grows the affordance matrix: `resume` on `stopped` and `error` only.
- `scripts/verify.sh` grows two cases — the two refusals — for 32 cases, and says in its own comment why
  the happy path is not among them.
- The **`stopped` producer remains open** and is the most visible gap in the phase: until or
   lands, `stopped` can be reached only through the state function. The next story,
  (delete), produces none either; **the eviction is the first producer**, the idle stop the
  second, and the demo's "stopped *with a reason*" half is theirs (this story's tests supply the reason
  themselves, and the resume clears it, which is what they assert).
- Retention (`stopped_ttl_days`) and the UI's "days remaining" line have the column they need:
  `stopped_at` moves with the state and is cleared by the resume, which was ADR-0016's choice and is
  proved here.
