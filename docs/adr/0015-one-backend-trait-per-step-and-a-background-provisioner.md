# One backend trait of single steps, one background provisioner, five progress events

HS-008 answers a create with 202 and a `starting` row, HS-010 streams the session's events, and
HS-009 commits them durably. None of them produced one. HS-011 is the first producer: the accepted
session has to become a running one, without Docker (ADR-0001), and the operator has to be able to
see which of the four steps is slow.

The story settles little of the shape. "A backend trait covers provision, start turn, steer, abort,
stop, resume, delete, snapshot" does not say what a step is, or whether one call covers all four;
"the provisioner runs in the background" does not say where it lives or what it emits; and the
`session.status` payload is never written down. Each of those is hard to change once five later
stories have built on it, so this records the choices and the evidence.

## Measured before deciding

- **An `async fn` trait is not object-safe.** `dyn SessionBackend` cannot be built, so `AppState`
  could not hold a `Box<dyn SessionBackend>`; the alternatives are an enum, or an `async_trait`
  dependency whose only job would be boxing futures. There is no implementation outside this crate,
  so an enum is one `match` per operation and no new package in `Cargo.lock`.
- **`async fn` in a public trait is a warning** (`async_fn_in_trait`), because the desugared future
  does not promise its auto traits. `-D warnings` is the clippy gate, so the trait carries one
  targeted `#[allow]` and a comment saying why: the compiler still proves `Send` at the one place it
  matters — `tokio::spawn` of the provisioner over `Backends`, a concrete type.
- **`MutexGuard` is not `Send`.** Every status write therefore *has* to be a synchronous block
  (ADR-0012/ADR-0013): the compiler is what keeps the lock out of the provisioner's awaits, and the
  event writer's commit can never wait behind this task.
- **The writer commits every 25 ms and broadcasts after** (ADR-0013). A row that already says `idle`
  can be a few milliseconds ahead of the event that reports it, which is why every test that reads
  the log waits for the events to be committed instead of asserting immediately.
- **`serve`'s `--backend` was the only way to choose a backend**, and the default is Docker, which
  does not exist yet. Left alone, a create in a default build would produce a session that says
  `starting` forever.

## One trait method per step, not one `provision()`

`provision(session, step)` — the backend owns what one step *is*, the provisioner owns the order and
the reporting. A single `provision()` that ran all four would put the progress events inside each
backend, where the Docker backend and the stub backend would each have to reproduce them; and the
failure point the story needs ("told to fail at a named step") would be a name that no caller can
see.

`Step` is `Worktree`, `Network`, `Container`, `Pi`, with `ALL` in the spec's order, and its one
spelling is `as_str` — the `Status` precedent (ADR-0011): a `Serialize` impl that goes through it
rather than a `rename_all` derive, so the wire word, the log word and the test's expectation cannot
drift. Parallel: `runtime::Backend` is which backend *this run* uses; `SessionBackend` is what a
backend does. Two things with a similar name, and the doc comment on both says which is which.

A tenth method, `undo(session, step)`, is not in the story's list and is there deliberately. The
failure path is "undoes completed steps", and `delete` — the one teardown the story does name — is
the whole session's removal (HS-019), which would also throw away the worktree of a session that
`error` can resume from (spec item 8). Per-step undo in reverse order is the shape HS-024 fills:
`docker rm -f`, `docker network rm`, `git worktree remove --force`. Best effort: an undo that fails
is logged and the rest still run, because leaving two partial resources behind is worse than one.

The stub records every call instead of creating something, which is what a backend with no resources
can honestly offer. `StubBackend::failing_at(step)` is the story's "the stub can be told to fail at
a named step"; the delay is a constructor argument and not a config key, because 200 ms per step
paces a dev instance and does not describe the machine (ADR-0006's table is machine settings).

## The provisioner is a spawned task wired from `AppState`

`AppState` gains `backend: Backends` — the value a background task can take with it, and the one a
test can replace with a failing stub. It is built once, from `runtime.backend`, so the run's choice
is the only authority and the provisioner never re-reads the config. `AppState::provisioner()` wires
the three handles (`backend`, the database's `Arc`, the event writer) in one place.

`http::sessions::create` spawns it after the row is committed and after the response is built, so
the 202 never waits for a step. `create_locked` now reports whether it *inserted*; only a new row is
handed off, because a retry of the same `Idempotency-Key` answers an existing session and a second
worker on one session would emit a second set of steps and race the first.

That leaves a row that was inserted and never provisioned — the process died in between. Spec item
7.5 ("any row still in `starting` is resumed or set to `error`") is HS-044's reconcile, not this
story's, and the row is visible and wrong in the one place a reader will look for it.

## The row moves first; the event follows

Five events on success:

```text
{"step":"worktree","status":"starting","reason":null}
{"step":"network","status":"starting","reason":null}
{"step":"container","status":"starting","reason":null}
{"step":"pi","status":"starting","reason":null}
{"step":null,"status":"idle","reason":null}
```

A step event is emitted *before* its step runs, because the story exists to make a slow start legible
*while* it is slow; it claims nothing about the row, only that the step is now the one running. The
terminal event is emitted *after* the row is written, and so is a failure: `idle` before the event
says `idle`, `error` with its reason before the event carries it. An event that arrives before its
row would tell a client something the database does not hold.

Three keys, in that order, with explicit `null` rather than an omitted key — the same rule the
session row follows, so a client decodes one shape. `step` is `null` on the terminal event because
no step is running then; on a failure it names the step that failed, which is the most useful word
the payload can carry.

Failure adds the steps that reached it and then one error: failing at `container` emits `worktree`,
`network`, `container`, then `container` with `status: error` and the reason. The stub's reasons are
sentences an operator can read ("the stub backend was told to fail at the container step"), and the
same sentence is stored in `sessions.status_reason`.

This is not the state machine. The provisioner writes exactly two transitions, `starting → idle` and
`starting → error`, through the repository's one status writer; HS-012 owns the table, the refusal
of an illegal move, `stopped_at`, and the payload's `from`/`to`. It also owns the event the
provisioner emits last: when the table exists, `status` becomes `to` and `from` is added, and that
change is one story's work in one file.

## `backend` is a config key, and the flag still wins

The story names `backend = "stub"` and `--backend`, so `Config` gains `backend: Backend`, one
`KeySpec` row of `Kind::String` parsed by the same two words the flag accepts. `serve` resolves
`args.backend.unwrap_or(loaded.config.backend)` — the precedence `--port` and `--data-dir` already
follow: the file is the machine's setting, the flag is this run's (ADR-0006). `Runtime` stays the one
authority at run time; the provisioner reads `AppState.backend` and never the file.

`Config::default().backend` is Docker, because the stub is opt-in (ADR-0001). Until HS-024 there is
no Docker backend, so `DockerBackend` refuses every operation with `BackendError::Unsupported` and a
reason naming the step. That is deliberate: a session created on the default backend ends `error`
with "the worktree step: the docker backend does not provision yet (HS-024)" instead of sitting in
`starting`. The lie a unit-struct no-op would tell is worse than the refusal, and `error` is not a
live state, so it does not hold a cap slot.

`scripts/verify.sh` therefore boots on a config file that says `backend = "stub"`, which is also the
acceptance proof for the config half; `tests/serve_cli.rs` covers the flag, the override and the
default.

## Consequences

- `scripts/verify.sh` grows two cases — `case_backend_from_config` (the banner says `backend stub`
  with no flag) and `case_provisioning_steps` (the stream's step words, and the event that ends the
  session `idle`) — plus `fetch_sse_until`, which reads body lines until one contains a needle: an
  SSE response never ends, and `fetch_stream` deliberately stops at the header block. 19 cases, 56
  assertions.
- `tests/support/mod.rs` gains the SSE reader that `tests/streams.rs` had privately, and a harness
  stub that takes a minute per step, so no test that is not about provisioning can lose the race
  against a provisioner HS-011 starts in the background.
- HS-008's `a_create_starts_no_provisioning` is amended rather than deleted: the create still does
  no provisioning itself, and `tests/provisioning.rs` owns the half that now happens.
- The stub leaves `worktree` `''`, per HS-007's note; HS-021 and HS-024 are what fill it.
- HS-012, HS-019 and HS-024 are the three stories that consume this shape. None of them has to move
  a file: the payload, the trait and the undo path are all where those stories will look.
