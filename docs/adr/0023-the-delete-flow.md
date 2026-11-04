# The delete flow: what the request does, what the task does, and what survives

HS-019 is the first story that destroys something, and the spec's one sentence about it (item 9's
"Delete flow (returns 202, state `deleting`): abort if running; snapshot; `docker stop`; `git worktree
remove --force`; remove the session directory and network; delete the row. If the snapshot fails,
delete returns 409 `unsaved_work` unless `?force=true`") contains a tension the code has to resolve:
the answer is *202, accepted*, and one of the list's steps must be able to answer *409, refused*
instead. Five decisions follow from that, and one more from what a delete does to the event log.

## The snapshot is the request's, and everything after it is a task's

The split is the point of no return. Before it the session is not being deleted: its row, its event
log and everything the backend owns are untouched, so a refusal leaves the session where the operator
found it — a running turn may have been ended by step 2, which is work the abort ladder would have
done anyway. After the transition the row says `deleting`, which the table has no way out of, so the
only honest exit is the removal.

So the handler, in order: read the row (`404` when there is none), end a running turn, take the
snapshot (`409 unsaved_work` unless `?force=true`), apply `any + delete → deleting`, answer 202 with
the row, and hand the session to a background task. `docker rm`, the worktree, the key and the row are
the task's, for the reason provisioning is a task's (ADR-0015): a tap must not wait for a container.

The alternative — snapshot in the task — cannot answer 409 at all, and a task that failed the snapshot
would have to either leave the row `deleting` (a stranded session with no way out) or apply a
transition the table does not have (a second state machine). Neither is worth the shorter request.

## A running session's turn is ended by the ladder, and the ladder does not resume it

`Ladder::climb` was already two rungs plus a way back. This story splits it: `end_turn` is rung 1,
the wait and rung 2; `climb` is `end_turn` followed by the resume when rung 2 fired. `climb` behaves
exactly as before, and the delete calls `end_turn` with the reason `deleted`.

Why the turn has to end at all: the snapshot runs `git add -A` against a working tree, and a tree an
agent is still writing is not a state anyone can snapshot meaningfully. Why the resume must *not* run:
rung 2 exists so that a killed container does not lose a session, but this session is being removed —
provisioning a container in order to delete it would be the most expensive kind of pointless, and with
a real Docker backend it would fail anyway.

The cost is that a delete of a *running* session may take up to `abort_grace` (ten seconds by
default) before it answers. That is the one request allowed to wait for a turn, and it waits only
because the alternative is discarding work it did not snapshot. A session that is `idle`, `stopped` or
in `error` answers immediately.

A rung-2 stop also means the container is gone, so the snapshot that follows may legitimately fail —
a Docker backend cannot `docker exec` into a dead container. That is the `409 unsaved_work` an
operator then forces past, and it is the truth rather than a bug: the work could not be saved. The
stub backend records calls instead of running them, so nothing in phase 1 reaches that path; HS-024's
real container will.

## `SessionBackend::delete` is the whole of the backend's teardown

The spec names four host-side steps (`docker stop`, `git worktree remove --force`, the session
directory, the network). They are one trait call, `delete`, because the trait already says what they
are — "Remove everything the session owns" — and because splitting them would give HS-024 five call
sites to keep in one order instead of one implementation.

A backend that **cannot** delete is a special case worth its own branch: `BackendError::Unsupported`
means the backend never created anything (today's Docker backend refuses provisioning too, which is
the only thing that variant is for), so the row is removed as asked and the branch is logged. Any other
failure means resources may still exist, so the row stays `deleting` — visible, and recoverable by
repeating the request, which re-runs the task.

## Idempotency is per row state, and the second answer is the recovery path

The bullet allows 202 or 404 "deterministically", so this story keys the answer to the row:

| The row | The answer |
| --- | --- |
| absent | `404 not_found` |
| `deleting` | `202` with the row, and the removal re-runs |
| anything else | the pre-flight, then `202` |

The `deleting` branch does no second abort, no second snapshot and no second transition: the point of
no return was crossed by the request that moved the row there. What it does is re-run the task, which
is what makes a removal that failed half way retryable — the alternative is a row nothing can reach,
since `deleting` is a sink in the table and every other endpoint refuses it.

## `?force=true` is the literal string, and a typo fails safe

The spec names one spelling. Anything else — a bare `?force`, `?force=1`, `?force=TRUE` — is not
force, so the answer is the 409 rather than the removal. There is no error code in the spec for an
unrecognised query, and inventing one would make a typo louder than the work it was about to destroy;
failing towards *not deleting* is the direction this endpoint must fail in.

`force` forgives a failed snapshot; it does not skip the attempt. Spec item 9 says the snapshot runs
before a delete, and a retry that quietly stopped trying would be a second rule for the same request.

## The event log goes with the row, and an event for a gone session is dropped

Deleting the row and keeping its events would leave rows nothing can read: every reader in the API is
keyed by a session that exists (`read_since` is reached through a `404`-checked route), so the log
would be dead weight that only a future vacuum removes. So `sessions::repository::remove` deletes the
log and the row in **one** transaction — the log first, through the events repository's own statement,
so no `events` SQL lives in the sessions repository.

That has a consequence in the writer, which is where this story had to change an HS-009 invariant. An
event is committed by bumping its session's `last_seq`, so an event whose session has gone has no seq
to carry. Until now `insert_batch` refused the whole batch with `UnknownSession`. That would have been
wrong twice over: the `deleting` transition is *itself* a queued event, and the removal happens
milliseconds later, so every delete would log `a batch of events was not committed` — and, because a
batch is whatever 25 ms of appends happen to share, it would take other sessions' events down with it.

So `insert_batch` now **drops** such an event and commits the rest. The precedent is `touch`'s "a
session it can no longer touch has nothing to record": a row that vanished underneath a producer is an
ordinary race now, not a producer bug. `RepoError::UnknownSession` goes with its last constructor.

## The provisioner has to notice, and the dummy key has a hook

Two smaller consequences, both about code that outlives the row:

- **The provisioner stops when the row leaves `starting`.** The table admits `delete` from every
  state, so a session can be deleted while it is being provisioned. Without a check the provisioner
  would build four steps' worth of resources for a session that is going away and then log an `error!`
  for the `pi_ready` the table refuses. It now looks at the row at each step boundary and returns
  quietly; nothing is undone, because the delete's `backend.delete` is what removes the resources.
  A step already in flight when the row moved is still a window, and closing it is HS-024's: a real
  `docker run` racing a `docker rm` is a question about the Docker backend, not about this flow.
- **Revoking the session's dummy key is a named no-op.** No session has a key until HS-039 issues
  one, and deleting the call site would leave "key revoked on delete" as a promise with nowhere to
  land. `deleter::revoke_key` is where HS-039 writes its code.

## Two deferrals, recorded so they are not rediscovered

**No UI.** The acceptance is API-only, and a destructive affordance needs a confirmation step and a
"the snapshot failed — delete anyway?" path, which is the sheet the hub owns (HS-057+). A page that
fired this request from a row would be the mis-tap the story's own "as/so that" is about.

**Nothing verifies that the work is recoverable.** A snapshot writes `refs/holosmith/wip/<id>`, and
spec item 9 says the branch and the ref survive the delete — but this flow removes the session's
directory, and with it the worktree the branch is checked out in. Recovery is `git branch rescue
refs/holosmith/wip/<id>` in the shared repository, which HS-024's Docker teardown must not break.

## Consequences

- `crates/holosmith/tests/delete.rs` proves: the 202 with `status: "deleting"` and the row gone
  afterwards (one row, the list, and the stream all answering as if it never existed, and the event
  log the removal took with it); the stream ending when the removal finishes; `409 unsaved_work`
  leaving the row `idle`, no `delete` call and no `deleting` event; `?force=true` removing it with the
  snapshot still attempted; the call order `clear_queue → abort → stop → snapshot → delete` for a
  running session; a delete of a session caught mid-provision, with no provisioning step after it; a
  repeat `DELETE` on a `deleting` row re-running the removal without a second snapshot; `404` after the
  removal and for an unknown id; `403 bad_client` without the client header; and the route recorded
  `Mutating`.
- `Ladder::end_turn` is the shape HS-022's turn timeout and HS-023's budget climb, and
  `the_ladder_ends_a_turn_without_bringing_the_session_back` pins both halves of its contract.
- `scripts/verify.sh` grows three cases — the round trip, the unknown id and the missing client header
  — for 35 cases; it also grows `wait_for_gone`, because a 202 is a promise and the row is the proof.
- The delete can leave a `stopped` row behind — a running turn it had to stop (reason `deleted`) and
then would not remove — but a delete that succeeds leaves no row at all, and HS-020's eviction is
still the first producer of a stop the operator did not ask for.
