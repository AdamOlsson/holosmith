# The drain is a socket and a process flag, and it is not a shutdown

This is spec item 10 step 3 — "Drain: `holosmith admin drain --wait 10m` over a local unix socket. It refuses new
creates and prompts (503 `draining`), waits for running turns, then stops idle sessions with reason
`server_restart`" — plus item 8's `idle → stopped` row, item 10 step 6, and the API table's `Admin unix socket`
row (*`drain` command, not reachable over HTTP*). Almost none of it is new mechanism: the transition table already
had `Trigger::Drain` and its one row, `Stopper` already existed for "the stop was ours", the startup reconcile
already brought a live row back, and the refusal envelope already had a shape. What this ADR records is the five
decisions that could each be taken a different way by a later reader.

## D1 — The command is `holosmith drain [--wait 10m]`; `holosmith admin drain` is the prose's older spelling

The spec spells the command twice. Item 10 step 3 says `holosmith admin drain --wait 10m`; the **Commands table**
(spec item 11) says `holosmith drain [--wait 10m]`, and the story's own acceptance — "`holosmith drain [--wait
10m]` talks to a local unix socket" — repeats the table. The table wins, for the reason: the Commands table
is the surface the spec presents as the command set, and took the table over the prose in the same
situation. There is **no `admin` subcommand group**: a group with one verb in it is a second spelling for one
command, and the non-goals leave every other admin verb out.

`--wait` is parsed by the **config's own** duration grammar (`crate::config::Duration`), so `10m`, `500ms` and `1s`
mean here what they mean in the config file, and a bare number or a zero is refused in the parser's words before
anything is loaded. The default is `10m`, which is spec item 10 step 3's own value.

## D2 — The socket is `<admin dir>/drain.sock`, and the admin directory is the only path both sides compute

`crate::deploy::ADMIN_DIR` (`/run/holosmith`) is step 2's directory: created **0700**, chowned to the service user,
re-created on every deploy because `/run` is a tmpfs. That is exactly the directory a socket needs, and
its comment already said "path fixed ". The socket is `drain.sock` inside it, from one
`drain::socket_path`, so a server and a client cannot disagree about a path.

`serve` and `drain` both take **`--admin-dir PATH`**, and nothing else selects it. Not a config key: the admin
directory is a fact of the machine's install tree rather than a setting (the spec's config list is exhaustive), and
`--data-dir`'s precedent is exactly this — a run's override that is not a key. Not `--root`: `--root` is a
`deploy`/`uninstall` word for an install tree, and ADR-0056 keeps it there; `--admin-dir` is the same override for
a run that is not the machine's, which is what a gate case and a demo use because there is no systemd here.

Three bind-time rules, each a measurement rather than a preference:

- **The directory is created 0700 only when absent.** A `serve` that is not the machine's may be the first thing on
  a tree that was never deployed; an operator's own mode on an existing directory is theirs (the data root's rule).
- **A dead socket is replaced, a live one is refused, anything else is left alone.** `UnixListener::bind` fails with
  `AddrInUse` for a socket file whether or not a server is behind it, so the path is probed first: `connect`
  refusing (`ECONNREFUSED`) means a dead server's file and it is unlinked; `connect` succeeding means another
  server owns this machine's admin socket, and unlinking would hand its clients to us, so this run warns and serves
  without one. A **regular file** at that path is an operator's, and the answer is a sentence naming it, not a
  delete — `connect` answers `ECONNREFUSED` for a regular file too (measured), so the file's own type decides
  before anything is removed.
- **A bind that fails is a `warn`, never a refusal to serve.** The control UI is what an operator needs when
  something else on the machine is wrong (ADR-0051's preview listener, same argument).

The socket is **0600** and the directory **0700**. There is no authentication of its own: on a deployed host only
the service user can open the file, so being able to open it *is* the authorisation, and the API table's "not
reachable over HTTP" is then a fact about the filesystem rather than a claim about a route.

## D3 — The wire is one request line, progress lines, and one `result:` line

```text
client → drain 600000            (milliseconds: the client already parsed the operator's `--wait`)
server → state: draining | state: already draining
         waiting: 2 turns still running        (only when the count changes, only when it is not zero)
         note: 1 session is starting and left alone, and comes back as idle after the restart
         stopped: 3 sessions with reason server_restart
         result: drained | expired | refused
```

Milliseconds rather than a unit suffix, because the client has already converted the flag with the config's parser
and a second duration grammar on the wire is a second thing to get wrong. The grammar is deliberately one verb — a
`status` verb would be the second admin verb §1.8's non-goals exclude, and `holosmith status` is where
that fact already lives.

**The terminal word is the exit code**: `drained` is 0 and everything else is 1. That is the promotion's contract
0 must mean "the machine is quiet, go" and 1 everything else, and the two arms that are
easy to confuse — a `--wait` that ran out and a socket that was not there — must not both read as success.
`expired` is deliberately not a success: acceptance bullet 4 says the expiry "returns non-zero and leaves the drain
state active rather than silently succeeding".

**A second drain joins rather than refusing.** The sequence then runs again against the same process: the flag is
already set (so the report says `already draining`), the wait is that client's own `--wait`, and the stop phase
finds nothing left. An operator reaching for the same tool twice is not an error, and a refusal would need a second
verb to ask "is one running?" — which D3 does not have.

**On the client side a read timeout is "nothing yet", not "gone".** The server is silent between progress lines by
design — it reports a change, and a turn that takes a minute changes nothing for a minute — so the client tolerates
silence up to the `--wait` it asked for (plus one `ANSWER_TIMEOUT` of grace) and only the **first** line is bounded by
`ANSWER_TIMEOUT` alone. Treating the first timeout as a dead socket made `holosmith drain --wait 1m` fail after five
seconds against any turn longer than that, which is the story's own demo; the bound that remains is the one that
matters — a socket whose server accepted and then said nothing at all is still refused.

**A dropped client does not abandon the sequence.** A write that fails is logged and the drain continues: the
report is the account, the state change is the thing, and stopping halfway would leave a machine that refuses work
and never stopped its idle sessions.

## D4 — `draining` is a bit on `AppState`, never a row and never persisted

`AppState.drain: drain::Control` is an `Arc<AtomicBool>` with three methods (`begin`, `is_draining`, and the
`Default` a boot starts from). Every clone of the state carries the same bit, which is the whole requirement: the
socket's task sets it and the request path reads it, and those are different tasks by construction.

A **database row** was the alternative and it is wrong twice: the flag would outlive the restart it exists to
precede, and the next boot would start draining with nobody asking. The drain is the *process's* state, and a
restart is the one thing that must clear it — which it does, because the new process builds a new `Control`. So
there is no un-drain command either: the way to stop draining is the restart the drain was called for.

The handlers that consult it are `POST /api/sessions`, `POST /api/sessions/{id}/prompt`, and
`POST /api/sessions/{id}/resume`. The ones that do not:

| handler | while draining | why |
| --- | --- | --- |
| `POST /api/sessions` | **503** | it starts a session, which is the work the drain exists to keep off the machine |
| `POST …/prompt` | **503** | it starts a turn (spec item 10 step 3's own two words: "creates and prompts") |
| `POST …/resume` | **503** | it provisions a container and starts a session: the same work as a create, by another door |
| `POST …/abort` | works | a refused abort would trap the turn the drain is waiting for; the ladder's resume is `resume`'s, not this handler's |
| `POST …/steer` | works | `steer` refuses unless the row is `running` (`NOT_RUNNING_STEER`), so it cannot *start* work — it can only shape a turn that is already ending, which is what an operator watching a turn wants |
| `DELETE /api/sessions/{id}` | works | a refused delete would trap a session the operator wants gone, and it ends work rather than starting it |
| every `GET` | works | acceptance bullet 2 says reads still work; the UI must be able to watch the turn the drain is waiting for |

Resume is the one addition beyond the acceptance's two words, and it is not an invention: a resume is a create's
work under a session's name, and allowing it would let a client defeat the drain one row at a time.

## D5 — The refusal is `503` with `code: "draining"`, the envelope family's first non-4xx code

`ApiError::draining()` is ADR-0008's envelope (`{"code","message"}`) with `503 SERVICE_UNAVAILABLE` and the code
`draining`. It is not a `409`: `not_in_state` says "this *session* cannot take that request", and this says "this
*machine* cannot take it right now, and the operator is the reason". It is not the `500 internal` family either,
which would say nothing can be done about it. The sentence names the drain and the `--wait`, because those are the
two things the operator needs: what is happening, and where to read the end of it.

The create's refusal rides the **refusal slot `slot_for` already carries**, which is
the ordering that matters: after the idempotency lookup (so a retry of a key still answers its own row) and before
the cap (so a refused create never evicts an idle session on its way to saying no). While draining, the project's
git questions are not asked at all — a draining machine starts nothing whatever the project looks like — and the
test `a_retry_of_an_existing_create_still_answers_its_row_while_draining` is that ordering, not a re-test of
idempotency.

## D6 — "Waits for running turns" is a poll over the rows, and `starting` is reported rather than waited for

The wait reads `sessions().counts_by_status()` under the existing mutex every 100 ms until no row is `running` or
the deadline passes. The row is the honest reader, and the alternative — the in-process turn tasks — is not:
a turn ends by moving the row (`turn.rs`, `ladder.rs`), a row is what a crash leaves behind, and a count of tasks
answers "no turns" for a machine whose database says otherwise.

`starting` is **not** waited for. The transition table has exactly one `Drain` row (`idle → stopped`), so a
provisioning session cannot be stopped by this policy without inventing a row, and item 10 step 6 already says
what happens to it: the startup reconcile brings a live row back to `idle` (ADR-0053). The report says so once
(`note: 1 session is starting and left alone, …`) rather than leaving the operator to wonder why the drain finished
with a container still coming up.

The deadline is the `--wait` value, and expiry **changes nothing**: the flag stays set, nothing is stopped, the
line says `this machine is still draining`, and the exit code is 1. A drain that cancelled itself on expiry would
be the "silently succeeding" the acceptance names.

## D7 — The stop is the table plus `Stopper`, and the drain is the fourth policy that stops something

For each `idle` row: `Transitioner::apply(&id, Trigger::Drain, Some(SERVER_RESTART))`, then the container's stop in
a spawned task through `Stopper`, which writes `sessions.expected_stop` **before** the backend's stop (,
spec item 6). Row first, container second — ADR-0020's order and every other policy's: the row is what the next
boot reads, so a wedged `docker stop` must not hold the command up.

`server_restart` lives in `drain.rs`, by the rule the reason words already follow: one constant per producer
(`deleter::DELETED`, `ladder::FORCED_ABORT`, `sweeper::IDLE_STOP`, `turn::TURN_TIMEOUT`/`BUDGET_EXCEEDED`,
`reconcile::INTERRUPTED`, `http::sessions::EVICTED`, `exit::PI_EXIT`). It is the fourth policy that stops
something, and the only one an operator asks for rather than a rule.

Two consequences worth stating, because both look like bugs if read from the other side:

- **A row the drain stopped is `stopped`, so the next boot's reconcile leaves it alone.** Acceptance bullet 5 —
  "sessions live at drain time return to `idle` on the next startup" — is about the rows a drain could *not* stop
  (a turn that outlasted the wait, or a `kill -9` right after one), and the reconcile is its mechanism.
- **A row that took a prompt between the read and the move is `running` again**, and `idle → stopped` refuses it.
  That is an ordinary race: it is logged at `info`, skipped, and left to the restart. (It cannot happen while the
  flag is set — the prompt is refused — so this arm is the one-millisecond window before the flag.)

## D8 — A successful drain does not end the process

Item 10's promotion is drain → backup → switch → `systemctl restart`, so the drain precedes the restart and does not
replace it. A drained machine keeps serving: reads answer 200, `GET …/events` streams, the abort ladder still ends a
turn, and `SIGTERM` is still what stops the process (`serve.rs`, unchanged: stop accepting, a bounded grace, exit 0).
The ADR says it because "drain" invites the assumption that the process goes away when it finishes.

## D9 — Every boot in a test or a gate passes `--admin-dir`, because the default is the machine's real socket

The default is `/run/holosmith/drain.sock` — a real path on a real host. A test or a gate case that boots `serve`
without `--admin-dir` therefore creates and binds the *machine's* admin socket, which is how this story first went
wrong: eight spawns across three files — `tests/db_migrations.rs`'s three, two direct spawns in
`tests/serve_cli.rs`, and three of `verify.sh`'s own boots (a fourth, `start_serve`, had been given the flag when it
was written, which is why the mistake was invisible in the case that uses it most) — all did, and the leftover `/run/holosmith` (0775, from an earlier session's by-hand `mkdir`) then failed
`tests/deploy.rs`'s `perms` check on the next run — a failure with no relationship to the code under test.

The rule is the one `--data-dir` already had: a boot that is not the machine's says so on its command line. And
because a comment is not a guard, the shape is the test's, not the code's: every spawn names the directory, and the
gate's `start_serve` passes `$WORK/run/holosmith` for all of them. The drain's own tests then exercise the socket on
a `TempDir` path, which is also what lets them run in parallel.

## What this ADR does not decide

The drain's *caller* — `upgrade`/`rollback` (those stories). A `status` line for the drain state (a second surface
for one fact, and `holosmith status` is deliberately a reader that does not ask the process anything new). A hub
forwarding the drain (phase 3). An un-drain, a second admin verb, and any UI affordance for draining: none
is asked for, and each would be a new surface for a state the restart clears.
