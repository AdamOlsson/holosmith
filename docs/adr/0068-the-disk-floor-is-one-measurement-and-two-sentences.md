# The disk floor is one measurement and two sentences, and only a container run is measured

This is spec item 14's first sentence: "`min_free_gb` (default 20): session creation and resume fail with 507
below it." Two of its three readers already existed — `serve` refuses to boot below the floor, and
`doctor --quick` prints the same finding because it is the same function — and ADR-0008 had already
written down that "a 507 for the free-space floor (item 14)" was coming. So this ADR decides only what a *request
path* adds: where the check runs, what it shares, what it says, what `0` means, and who else is measured.

Five decisions, one of which is a change to a shared type.

## D1 — the floor is asked in the two handlers, and the create's answer is raised where its other refusals are

The create already collects one `refusal: Option<ApiError>` before `slot_for` — the drain's `503` or
the project's three `422`s — and `create_locked` raises it **after** the idempotency-key lookup and
**before** the cap. The floor joins that value, second: `drain` → floor → project.

That placement is the whole of the create's behaviour, and every consequence is wanted:

- a full machine is answered as a **machine** fact, so `refuse_project` is never reached and no `git` process
  runs for a request that cannot start;
- **no row is written**, so the provisioner has nothing to walk, `back_up` (which lives inside the
  `Ok((session, inserted))` arm) takes no bundle, and the idempotency key names nothing — a retry after the
  operator frees space creates the session normally;
- an idempotent **replay** still answers the row it already has, because the key's lookup precedes every
  refusal. The alternative — answering 507 for a request that created a session a minute ago — would lose work
  that was created while there was room, which is the one thing the retry rule exists to prevent. The *answer* is
  what is deferred, not the work: the handler measures first and `create_locked` decides, so a replay pays one
  `df` it then discards. Two milliseconds, and the alternative — reading the key twice, or letting the floor
  decide before the retry rule does — costs correctness or a second source of truth.

The order is witnessed, not asserted: `tests/disk.rs`'s cap test runs one live session at `max_live_sessions = 1`,
crosses the floor, and asserts the create answers `507` **and** that the idle session is still `idle`. Raised after
the cap, that same create would evict it on its way to saying no.

The resume asks it after its state check and **before** `refuse_project` and the cap, which is the mirror of the
create's order: the floor is asked before the project's git read and before the slot question. When the cap and
the floor both hold, the answer is the `507`: a slot is not worth freeing if there is no space to write in, and
nothing has moved, so the row is still resumable once space is freed.

This is ADR-0060's "a machine fact before a session fact", which `409 draining` and `403 publish_disabled`
(ADR-0066 D4) already follow — with one refinement: the *refusal* is deferred past the idempotency lookup, as the
drain's is.

## D2 — one measurement, two sentences

A new `disk` module holds the number: `available_gb` (the pure arithmetic on a `df -Pk` answer), `verdict` (the
comparison against the floor, as a named `Verdict` rather than a `bool`), and `measure` (the subprocess, blocking,
returning `Result<u64, DiskError>` carrying exactly the pieces each sentence needs). `preflight::disk` becomes one
caller of it and keeps its four sentences and its three outcomes byte for byte; `doctor` keeps reading
`preflight::report_with`; the two handlers compose **their own** sentence from the same number.

The request path's half lives in `http/disk.rs` rather than in `http/sessions.rs`, and that is a consequence of
the guard below rather than tidiness: `http/sessions.rs` holds `prompt`, `abort` and `steer` beside the two starts,
so exempting it from the "only a start reads the disk" rule would exempt exactly where a disk read on a turn's path
would be added.

The story's acceptance is the reason: "`doctor` reports the same measurement, so the two never disagree". That is
a property of the *measurement* — the parser, the kibibyte-to-gigabyte division and the comparison — and after
this ADR there is one of each, so disagreeing would take an edit rather than an oversight. The *sentence* is
deliberately not shared: the boot's fix is "install coreutils" or "there is no data root yet, which is not a disk
problem", while an operator who tapped *create* needs "free some space". ADR-0008 already says a message must
describe its own request, and `cap_reached`/`cap_without_idle` is the same split one code over.

Two smaller consequences, both measured rather than assumed:

- `DiskError::Missing` is a *variant*, not a `fail`: `doctor --quick` runs before `server deploy` has created
  anything, and answering "the disk is full" for a directory that does not exist yet would make a fresh host fail
  the check `serve` passes (ADR-0054's arm, now stated in the measurement instead of in a sentence).
- On the request path that arm — and `Refused`/`Unrunnable`/`Unprintable` with it — is **not** a 507. There is no
  number to report and "free some space" would be a lie about a machine nobody measured, so it is the envelope's
  500 with its reason in the log. On `serve`'s path those arms cannot be reached: the boot refuses first, with
  the same measurement.

## D3 — only a run that provisions containers is measured

`disk::guarded(backend, role)` = `backend == Docker && role != Hub`, and it is written once because three places
ask it: `serve`'s boot, and the two handlers. It is `preflight::check`'s own documented predicate — "a `stub` run
needs no daemon and no free space beyond its own database, so only a Docker run is checked", and a `hub` runs no
sessions at all (item 10 §6's deploy skips the Docker checks) — one level down: the floor guards a *container's*
use of the filesystem. Build caches and a worktree are gigabytes; a stub session starts no container.

The handlers read `runtime.backend` rather than `config.backend`, because `Runtime` is "the backend for this
invocation" and it is what `serve` built `Backends` from; the two agree on every boot `serve` performs.

**One consequence, stated because it is where the acceptance's third bullet stops.** `doctor` never consults the
backend — it skips the Docker and git checks on a hub and nothing else — so on a *stub* run `doctor --quick` will
fail its `disk` line while `POST /api/sessions` answers `202`. That is the same split as the boot: a stub `serve`
starts on a full disk and refuses nothing, which decided. The two readers still agree about the
*measurement*; they disagree about whether it applies to this run, deliberately.

This is also what keeps the test suite honest. This sandbox's `/tmp` has 3 GB free and the default floor is 20, so
a backend-blind floor would turn every stub create in `cargo test` into a 507 — and teaching 26 test files to say
`min_free_gb = 0` would hide the code path from every test that exercises it.

## D4 — `min_free_gb = 0` is legal and means "no floor"

The comparison is `available >= min`, so `0` always passes — and `0` goes further than that: the request path
**does not measure at all** when the floor is `0`, so a machine without a working `df` is not refused a session
over a floor it does not have. (Measuring first and comparing afterwards would answer `500 internal` for
`DiskError::Unrunnable`, which is what `tests/disk.rs` catches: its zero-floor test points `df` at a path that does
not exist.) Refusing `0` in `config validate` would be a new config rule for a story that asked for a refusal, and
`tests/serve_cli.rs` already writes `0` to say exactly this. It is documented in `GLOSSARY.md` instead of being
re-keyed.

## D5 — the sentence carries the numbers, so the envelope's message may be owned

The request's sentence is:

> `<data root>` has `N` GB free and `min_free_gb` is `20`, so this machine starts nothing: free some space, or lower
> `min_free_gb` if this machine really has room for fewer sessions

It is **one sentence for both requests**, and it therefore says "starts nothing" rather than "no new session":
a create would make another session and a resume brings an existing one back, so a sentence naming a *new* session
would describe only half its callers. That is not the `cap_reached` split — there the two requests have two
different next steps, while item 14 gives a create and a resume one fix (free some space, or lower the key).

Carrying `N` is the point: the operator reading a refusal on a phone needs to know *how much* to free, and the
boot already tells them. Until this story every `ErrorBody.message` was a `&'static str` and every constructor
passed a literal, which the module's own comment recorded. The field is now `Cow<'static, str>` and
`ApiError::new` takes `impl Into<Cow<'static, str>>`, so:

- every existing caller compiles unchanged (a `&'static str` converts);
- the serialised bytes are identical, so no client sees a difference and no test's envelope changed;
- `internal` keeps its fixed sentence, so ADR-0012's rule — no rusqlite text on the wire — is untouched;
- the owned arm is used by exactly one sentence, so the envelope is still allocation-free for the rest.

The alternative was a fixed sentence with no numbers, which is smaller but leaves the operator's decisive question
unanswered by the very refusal they are reading.

## Consequences

- `507` + `disk_floor` is answered by `POST /api/sessions` and `POST /api/sessions/{id}/resume`, and by nothing
  else: a gate case asserts that a read still answers and never carries the code, and a source-level guard asserts
  that `disk::measure` is named outside `disk.rs` only in `preflight.rs` and `http/sessions.rs` — so "running turns
  are not interrupted by crossing the floor" is a fact about the tree rather than a claim. `tests/disk.rs`
  additionally runs a turn across the crossing and watches it settle.
- A measurement that cannot be made on a request path answers `500 internal`, not `507`.
- Item 14's remaining sentences (`session_disk_soft_gb`, the `du` watchdog, the `disk_quota` event) belong to
   and are unaffected by this split: the watchdog measures a *session's* usage, not the machine's floor.
