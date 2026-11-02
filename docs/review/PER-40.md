# Review — PER-40 (HS-008)

**Fixed point:** `fad974d` (the last commit before this work), reviewed against the **uncommitted**
tree. In-scope files read in full: `crates/holosmith/src/http/sessions.rs`, `src/http/app.rs`,
`src/http/guard.rs`, `src/sessions/{mod,repository}.rs`, `src/db.rs`, `src/clock.rs`,
`src/http/error.rs`, `src/commands/serve.rs`, `tests/{support/mod,sessions_api,http_contract,
session_repository,db_migrations}.rs`, `scripts/verify.sh`, `docs/build.md`, ADR-0008/0009/0012,
`HolosmithSpec.md:123-160,297-308`, the HS-008 slice and the approved plan.

**Verification basis.** This role has no shell, so it did not execute the gates; it read the diff
artifact and the tree. The supervisor ran, after sourcing the toolchain env: `cargo fmt --check`;
`cargo clippy --all-targets -- -D warnings`; `cargo test` (12 binaries: 160 lib, 20 config_cli, 12
db_migrations, 39 guards, 11 http_contract, 2 pin_single_source, 12 serve_cli, 20
session_repository, 20 sessions_api, 4 version_cli); `scripts/verify.sh` end to end (exit 0, 15 cases
+ the CORS sweep, 46 assertions); and a seven-case `curl` demo against the shipped artifact — before
and after the fixes below.

**Verdict:** Standards OK with notes, Spec OK with one note. No blocker. One should-fix and five nits
were fixed; the one spec-side note is a handoff recorded in ADR-0012, not a code change.


Read: the brief, `PER-40-diff.txt` (2432 lines), and the live tree (`src/http/sessions.rs`, `src/http/app.rs`, `src/http/guard.rs`, `src/sessions/{mod,repository}.rs`, `src/db.rs`, `commands/serve.rs`, `tests/{support/mod,sessions_api,http_contract,session_repository,db_migrations}.rs`, `scripts/verify.sh`, `docs/build.md`, `ADR-0008/0009/0012`, `HolosmithSpec.md:123-160,297-308`, the HS-008 slice, the plan). Read-only: nothing was edited. Gates were not re-run here, and none are required.

### Answers to the five questions the brief asks me to weigh

All five hold; the evidence is cited against the findings where relevant.

1. **Envelope on every error path.** Every path *inside* the handler is `{"code","message"}` + `application/json`, including the three a `Json<T>` extractor would have flattened — `parse_body` (`src/http/sessions.rs:56-64`) turns non-JSON, non-object, missing/wrong-typed fields into `bad_body`, unknown keys into `unknown_field`, and `assert_refusal` (`tests/sessions_api.rs:78-99`) asserts status **+ content-type + exactly two keys + non-empty message** over a real socket for all nine codes. `axum::Json` is used only as a *response* (`sessions.rs:250-258`). One hole remains at the extractor — finding S1.
2. **No `std` guard across an `.await`.** `create` is the only async fn (`sessions.rs:176-188`); the lock lives inside the sync `create_locked` (`:190-237`), which contains no `.await`, so the guard drops before `created()` builds the body. Poisoning is recovered (`:191-194`, `PoisonError::into_inner`), not answered 500. Test helpers (`tests/sessions_api.rs:locked_row/live_count/set_status`) are sync fns too.
3. **Idempotency before cap.** `by_client_key` at `sessions.rs:198-203` returns before `live_count` at `:204-209`. Non-vacuous: `a_retry_of_the_session_that_filled_the_cap_still_answers_202` fails if the two statements swap. Cap `>=` and `u64::from(u32)` (`:207`) are correct.
4. **`DuplicateClientKey` mapping.** Mapped by `extended_code == SQLITE_CONSTRAINT_UNIQUE` (`repository.rs:78-87,187-190`), never by message. Two independent proofs: constructed `ffi::Error`s (2067 fires; 1299 and a non-SQLite variant do not — `repository.rs:258-283`) and a *real* UNIQUE failure on a real database now asserting the variant (`tests/session_repository.rs:354-372`). Not vacuous.
5. **The 500 does not leak.** `ApiError::internal()`'s message is a fixed sentence (`error.rs:222-231`); the operation + `%error` go only to `tracing::error!` (`sessions.rs:240-248`). No test drives the branch (nothing in `sessions_api.rs` names `INTERNAL`), but leak-freedom is structural, not behavioural — a constant cannot carry the path.

---

## Standards

**Correct (verified, no finding):** one lock scope covering lookup+count+insert (`sessions.rs:190-237`); no SQL outside the repository (`live_count` and `by_client_key` in `repository.rs`, guard test at `:220-262` still walks `src/**`); `AppState` construction moved at all three sites (`commands/serve.rs:114-121`, `tests/support/mod.rs:80-101`) and `state()`'s signature held, so the 45 call sites did not move; `verify.sh`'s `fetch` and the eleven prior cases are byte-identical, `fetch_body` sends `Content-Length` then `Connection: close` then the body (`scripts/verify.sh:158-181`); no crate added; `Status` serialises through `as_str`, pinned for all six (`sessions/mod.rs:73-79,185-196`).

- **S1 — should-fix (low; ADR-0008 contract).** The route's only fallible extractor is `Bytes` (`src/http/app.rs:282-285`), and its rejection is *not* the envelope. `axum-core 0.5.6/src/extract/rejection.rs:44-48` maps `UnknownBodyError` to `400 BAD_REQUEST` with the plain-text body "Failed to buffer the request body"; that fires whenever the body cannot be buffered at all (truncated `Content-Length`, malformed chunked framing, client reset mid-body) — a path a retrying phone will produce. ADR-0012 records only the sibling `LengthLimitError` 413 (`rejection.rs:36-42`), which the brief already accepts. So ADR-0008's "every error response that carries a body is `application/json`" has one unrecorded exception on the first write endpoint. Smallest fix: one sentence in ADR-0012 next to the 413 ("a body hyper cannot buffer answers axum's 400 `text/plain`"), or take `body: Result<Bytes, BytesRejection>` and map the rejection to `ApiError::bad_body()` — noting that mapping would also convert the known 413 into a 422.
- **S2 — nit (stale doc).** `crates/holosmith/src/db.rs:45-49`: the `Database` struct and field docs still say "sharing the connection between handlers is the single-writer decision HS-009 owns, **not this story's** (ADR-0009)" and "sharing it between tasks is the single-writer decision HS-009 owns". The diff updated `sessions()`'s doc (`db.rs:101-104`) but not these two sentences, which now contradict the code shipping beside them. Smallest fix: the same one-clause edit the plan's step 7 applied to `sessions()`.
- **S3 — nit (test isolation).** `tests/support/mod.rs:35`: `TempDir::new` now builds `holosmith-test-{label}` with no pid, replacing three pid-suffixed copies (`src/sessions/repository.rs:335-337` still appends `process::id()`). Two concurrent runs of one test binary — or of *different* binaries that both use `state()`'s `"shared"` label, `tests/guards.rs` and `tests/http_contract.rs` — now `remove_dir_all` each other's live directories and the open `app.db`. One `cargo test` invocation is serial, so this needs two invocations; the stale-dir cleanup motive (plan §2 step 8) is real, so the choice is defensible. Smallest fix: either append `process::id()` and accept one leaked directory per run (what the old copies did), or state the "one test process at a time" assumption in the doc comment at `:26-31`.
- **S4 — nit (in-file convention).** `crates/holosmith/src/sessions/repository.rs:123-133`: `live_count`'s statement is inlined, while every other statement in the file is a named `const` at the top (`COLUMNS:17`, `NEW_ID:22`, `INSERT:26`, `UPDATE_STATUS:33`, `ORDER:39`). A reader looking for "the statements this file owns" now finds one in the middle. Smallest fix: hoist it to `const LIVE_COUNT: &str = "SELECT count(*) FROM sessions WHERE status IN (?1, ?2, ?3)";`.
- **S5 — nit (latent harness bug).** `scripts/verify.sh:166`: `request+="Content-Length: ${#body}"` counts *characters*, not bytes, and `${#var}` is locale-dependent. Every body passed today is ASCII, so the run is correct — but the first non-ASCII body in a future case would send a short `Content-Length`, hang the reader to the 10 s `timeout`, and fail with a misleading `…/no_answer`. Smallest fix: `Content-Length: $(printf '%s' "$body" | wc -c)`.

**Standards verdict: OK with notes.** No blocker and no behavioural defect in the shipped paths; S1 is the one thing that contradicts a written contract (ADR-0008) and it is the same class ADR-0012 already records for the 413. S2/S4 are one-line edits, S3 and S5 are latent-only.

---

## Spec

**Correct (verified against the six boxes):** box 1 — `HolosmithSpec.md:123` / story `:39`; shape rules + `unknown_field`, each with a unit table and a socket-level test that asserts status, content-type and code. Box 2 — `:40`; row is `starting` with `client_key` (`repository.rs:26-31` writes `Status::Starting` and `?7`), the retry answers the same row (`the_same_key_answers_the_same_session` compares the whole body, not just the id), and the key is the identity (`a_different_body_with_the_same_key_is_the_same_session`). Box 3 — `:41`; absent *and* empty both `422 missing_idempotency_key`, each asserted together with `live_count() == 0`, so "not silently accepted" is proved by the missing row. Box 4 — `:42` + item 8; `live_count` counts exactly `starting|idle|running`, proved by walking all six statuses (`tests/session_repository.rs:416-441`) and by `stopped_sessions_do_not_count_towards_the_cap`. Box 5 — `:43`; 202, `Location`, `events` = `/api/sessions/<id>/events`, matching HS-010's path, and the body is pinned to the fifteen column names + `events` in order, twice (`sessions/mod.rs:198-...`, `sessions.rs:...`, `sessions_api.rs`). Box 6 — `:44`; `a_create_starts_no_provisioning` reads the row back after the answer. The demo is lifted to the shipped artifact (`scripts/verify.sh:363-440`), and no scope creep beyond the plan (the `clock` move and the `TempDir` lift are steps 1 and 8).

- **P1 — nit (slice gap, not this diff's behaviour).** `sessions.rs:251-257` answers `Location: /api/sessions/<id>`, but nothing serves that URL: the API table (`HolosmithSpec.md:297-308`) lists no `GET /api/sessions/:id` — only `/events`, `/changes`, `/tool-output` and the mutating verbs — so a client that follows `Location` today gets ADR-0008's `404 not_found`. Story box 5 (`phase-1-session-lifecycle-stub.md:43`) demands the header, and this is the right value for it, so the defect is in the slice: no story visibly owns a fetchable session URL. Smallest fix: one clause in HS-013's acceptance (`GET /api/sessions/:id` lands with the Sessions screen) or a non-goal in the plan saying the header is forward-looking. Reported once, as a spec-side note; not asking for a code change here.

**Spec verdict: OK with one note.** All six acceptance boxes and all three demo cases are implemented, and each has a test that would fail if the behaviour were removed — including the two easy-to-fake ones (the envelope asserted as envelope, not as status; the cap asserted as `live_count` unchanged, not as a 409 alone). P1 is a handoff detail about a URL the spec never promised.
## Resolution

No blocker. One should-fix and five nits fixed; the single spec-side note is a handoff recorded in
ADR-0012 rather than a code change. The should-fix was proved non-vacuous by mutating the fix and
watching the new test fail with axum's own `413 … Failed to buffer the request body`.

| Finding | Status | How / why |
| --- | --- | --- |
| **S1** the `Bytes` extractor's rejection escapes ADR-0008: `UnknownBodyError` answers a bodiless `text/plain` 400 (and `LengthLimitError` a `text/plain` 413), which fires whenever a body cannot be buffered at all — a truncated `Content-Length`, malformed chunked framing, a client reset mid-body, exactly what a retrying phone produces | **fixed structurally, not documented away** | The route now takes `body: Result<Bytes, BytesRejection>` and every rejection maps to `ApiError::bad_body()`, so no extractor failure can reach a client as plain text. An over-limit body therefore reads `422 bad_body` rather than `413`, which ADR-0012 records along with why (no 413 code, no configured limit, and a refusal with a code is more use than a bodiless one). A new test sends 2 MiB + 64 KiB and asserts status **and** code **and** `content-type: application/json` **and** the two-key envelope **and** `live_count() == 0`. **Non-vacuity proved**: restoring `rejection.into_response()` makes it fail with `left: 413, right: 422`. |
| **S2** `src/db.rs`'s `Database` struct and field docs still said the sharing decision was "not this story's (ADR-0009)", contradicting the code beside them | **fixed** | Both now carry the same clause as `sessions()`: the connection stays private, a handler reaches it only through ADR-0012's `Arc<Mutex<Database>>` stopgap, and HS-009's single writer owns the write path. A doc comment that cites a superseded rule is how a later reader concludes the opposite of what the code does. |
| **S3** the lifted `TempDir` dropped its pid suffix, so two concurrent `cargo test` runs — or two binaries sharing `state()`'s label — would `remove_dir_all` each other's live directories and open database | **fixed** | `process::id()` is back in the name, the `remove_dir_all` before create stays (which is what makes a stale directory harmless), and the doc comment says the pid is what makes two concurrent runs safe. |
| **S4** `live_count`'s statement was the only SQL not hoisted to a named constant | **fixed** | `const LIVE_COUNT` sits beside `COLUMNS`, `NEW_ID`, `INSERT`, `UPDATE_STATUS` and `ORDER`, so "the statements this file owns" is one place a reader can look. |
| **S5** `fetch_body` computed `Content-Length` from `${#body}`, which counts characters, not bytes | **fixed** | `$(printf '%s' "$body" \| wc -c)`. Every body today is ASCII so no run was wrong, but the first non-ASCII body would have sent a short length, hung the reader to the 10 s timeout, and failed with a misleading `…/no_answer` — a latent bug that would have cost an afternoon to diagnose. |
| **P1** `Location: /api/sessions/<id>` points at a URL nothing serves: the API table lists no `GET /api/sessions/:id` | **recorded, no code change** | Story box 5 demands the header and this is the right value for it, so the gap is in the story slice rather than this diff. ADR-0012 now says the header is forward-looking and that the fetchable session URL arrives with HS-013's Sessions screen, which is the first place the UI needs to re-read a session. Adding the route here would be scope the story does not have. |

After the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (12
binaries, all green) and `scripts/verify.sh` (exit 0, 15 cases + the CORS sweep, 46 assertions) pass,
and the seven-case `curl` demo against the shipped artifact still holds: 202 with `Location` and the
`events` URL, the same key answering the same id, three 422s with their own codes, 409 `cap_reached`
at the cap, and a retry of the session that filled the cap still answering 202.
