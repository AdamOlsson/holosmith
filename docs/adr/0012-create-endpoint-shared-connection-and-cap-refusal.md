# `POST /api/sessions`: one shared connection behind a mutex, validation by shape, and a cap that refuses

HS-008 is the first endpoint that writes. Three deferred decisions come due in it at once. ADR-0009 deliberately kept the `Database` out of `AppState` and left "how a handler reaches the connection" to HS-009; item 7's first step — "validate synchronously … insert the row" — needs it now, one story early. Item 7 also fixes the answer's shape: 202 with the session, then provisioning in the background, which is what removes the conflict between the hub's five-second timeout and a slow start. The story's third box forbids silently accepting a create without an `Idempotency-Key`, because retries are the whole point. And the spec never enumerates models or thinking levels while item 8's cap counts live sessions — in a tree where nothing can yet stop, delete or evict one.

So this records four decisions made together: how a handler reaches the connection without pre-empting HS-009, what "valid" means when the spec supplies no vocabulary, what the cap does when nothing is evictable, and how a repeated key is answered without a race.

## Measured before deciding

- **`rusqlite::Connection` is `Send` and not `Sync`.** rusqlite 0.37.0's `src/inner_connection.rs:44` is `unsafe impl Send for InnerConnection {}`, and the crate has no `Sync` impl for it. axum's `Handler` bound requires the future a handler returns to be `Send`, and this handler is a closure that captures `Arc<AppState>` across its `.await`s, so `AppState` must be `Send + Sync + Clone`. `Arc<Mutex<Database>>` is all three; a bare `Database` is not.
- **axum 0.8.9's `Json` extractor does not answer ADR-0008's envelope.** `src/extract/rejection.rs:12-39` defines `JsonDataError` (422), `JsonSyntaxError` (400) and `MissingJsonContentType` (415), each with a fixed `text/plain` body ("Failed to deserialize the JSON body into the target type"). A handler taking `Json<CreateBody>` would therefore answer a 422 that is not `{"code","message"}` — the contract `tests/http_contract.rs` already pins for every other error.
- **`SQLITE_CONSTRAINT_UNIQUE` is 2067, and it is reachable.** libsqlite3-sys 0.35.0 binds `pub const SQLITE_CONSTRAINT_UNIQUE: i32 = 2067` (`bindgen-bindings/bindgen_3.14.0.rs:109`), and rusqlite 0.37.0 exposes `Error::sqlite_error()` whose `ffi::Error` carries `extended_code`. `Error::sqlite_error_code()` answers only the primary code (`ErrorCode::ConstraintViolation`), which a `NOT NULL` violation produces as well, so the extended code is what separates "this key already exists" from "this insert is broken".
- **ADR-0011's measurements are what make the backstop cheap:** `client_key` is `TEXT UNIQUE`, many `NULL`s are accepted because `UNIQUE` treats each `NULL` as distinct, and a duplicate fails with `UNIQUE constraint failed: sessions.client_key`.
- **axum's body limit is 2 MiB, and every `BytesRejection` is mapped to `bad_body`.** The route takes the body as `Result<Bytes, BytesRejection>` and turns each rejection into the envelope's 422, so a body over the 2 MiB default reads as `bad_body` rather than 413 — there is no 413 code and no configured limit, and no extractor failure escapes ADR-0008. No limit is set or claimed as enforced.

## The handler holds the connection for one synchronous block

`AppState` gains `database: Arc<std::sync::Mutex<Database>>`, and `commands/serve.rs` wraps the `Database` it already opens and migrates, after the startup banner has read its path and schema version, so the banner needs no lock. Constructing the state stays in the one place it always was.

Why a `std::sync::Mutex` and not `tokio::sync::Mutex`: every repository call is synchronous and short — one statement — so there is nothing to await inside the critical section, and an async mutex would buy only the right to hold it across an `.await`, which is the thing this design forbids. The rule is therefore the one that makes the stopgap safe: **the guard is taken, the repository calls run, the guard is dropped, and nothing awaits inside that scope.** The compiler enforces it rather than a review note: `MutexGuard` is not `Send`, so a handler that held one across an `.await` would not produce a `Send` future and would not compile into the served app at all. That is why ADR-0009 could leave this open without risking a deadlock in the meantime.

The block is also the story's one piece of concurrency control that is a benefit rather than a cost: `live_count()` and `create` run in the same scope, so two creates cannot both observe a free slot and fill it. That is atomic within the process, which is all one data root has while ADR-0009's startup lock allows exactly one migrating `serve`.

A poisoned lock is recovered (`PoisonError::into_inner`) rather than answered: the connection is still open, every repository call is a single statement that leaves no transaction behind, and a permanent 500 for every later request would be a worse outcome than a possibly-stale connection.

**This is a stopgap and is recorded as one.** ADR-0009 deferred the sharing decision to HS-009, whose single writer task with group commit owns the write path; HS-009 replaces this `Mutex` rather than living beside it. Nothing in this story should be read as the final design: it is the smallest thing that lets item 7's synchronous validation and insert exist without deciding item 5's writer.

## Validation is shape, not vocabulary

The body is `{project, model, thinking, network}` and validation is four shape rules:

- `project`: non-empty, at most 255 characters, `[A-Za-z0-9._-]`, and not `.` or `..`. The bound is `NAME_MAX`'s 255, because HS-025 builds `<projects_root>/<project>` from it; whether the repository exists there is HS-025's check and HS-026's refusal set, not this story's.
- `model`: non-empty, at most 128 characters, `[A-Za-z0-9._:/-]`.
- `thinking`: non-empty, at most 32 characters, `[a-z0-9_-]`, starting with a letter.
- `network`: a JSON boolean, defaulting to `false` when absent — unconditionally, even when the config sets `network_default = true`, because that key is item 2's *checkbox* default for the UI (HS-013, HS-038) and an API body that omits the field is asking for no egress.

**No list of models and no list of thinking levels is invented here.** The spec names neither, and a list written now would be a third vocabulary: pi's own values, the config's, and this crate's. HS-031 is where pi's real values are confirmed and item 2's base-URL override is settled, so a list written today would either refuse a model pi accepts or accept one it does not — and it would have to be corrected in the one place a client can never see why. Shape rules refuse what cannot be a value at all (empty, absurdly long, a path separator in a thinking level) and leave what pi does not accept to the step that asks pi.

Rejections are the ADR-0008 envelope, all 422 except the cap: `bad_body` for a body that is not JSON, is not a JSON object, or is missing a required field or has one of the wrong type; `unknown_field` for a key the body does not allow; `missing_idempotency_key`; `bad_idempotency_key`; `bad_project`; `bad_model`; `bad_thinking`. One deterministic order decides which code a request with several problems gets — body parse, unknown field, idempotency key, then project, model, thinking — and a test pins it, because an order that is not written down is an order that changes when the code is re-arranged.

The handler reads the raw body and calls `serde_json::from_slice` itself instead of taking `Json<CreateBody>`, for the measured reason above, and it checks the object's key set against the four allowed names before deserialising, so `unknown_field` is a decision rather than a string match on serde's error text. The cost is a real one and is recorded: `ErrorBody.message` is `&'static str` (ADR-0008), so these messages are one fixed sentence per code and **`unknown_field` cannot name the field it refused**. Naming it needs the message type to carry owned text, which is a change to ADR-0008's struct and belongs to a story that edits that ADR deliberately, not to a side-effect of adding an endpoint.

## The cap refuses; eviction is HS-020's

`max_live_sessions` counts `starting`, `idle` and `running` — item 8's "live sessions". The count is a repository method of its own (`live_count`), one statement over the existing `sessions_status` index, and deliberately not `list` with three filters folded together: a count is one row of one column, and the three-status set is the thing HS-020 will also change.

When the count is at the cap, this story answers **409 `cap_reached`**, unconditionally. That is not an omission, it is the honest state of the tree: eviction needs a stop flow (HS-019) and eviction rules (HS-020), and neither exists, so there is nothing evictable. HS-020 refines exactly this branch to "evict the least recently active `idle` session, then proceed, and answer 409 only when nothing is evictable"; when it lands, this endpoint's refusal survives only for the all-`running`-or-`starting` case. Writing the whole rule now would mean writing a stop path in a story that does not own one.

The *order* matters as much as the answer: the idempotency lookup runs **before** the cap count. A retry whose first attempt filled the last slot must get 202 with its own session, not 409 — a machine at the cap has not made the client's retry invalid. A test pins it.

## Idempotency is a lookup, with the unique column as the backstop

A create must carry exactly one `Idempotency-Key` header. Its value is stored verbatim in the unique column `client_key`; the header's bytes are not folded, normalised or trimmed beyond the whitespace HTTP already strips. Absent or empty answers 422 `missing_idempotency_key` — the story's third box — and a value over 200 characters, one that is not text, or the header sent twice answers 422 `bad_idempotency_key`. Two hundred is generous for a UUID and small enough that the column cannot become a payload.

The normal path is a lookup: `by_client_key` first, and a hit is answered 202 with that session, whatever state it is in. The insert is still the authority, because two requests can pass the lookup at once across processes: when the insert fails on `SQLITE_CONSTRAINT_UNIQUE`, the handler re-reads `by_client_key` and answers the row that won. A retry must never see an error for a duplicate it created, so neither a 500 nor a 409 is an acceptable answer to a race the client cannot see or fix. The repository maps the unique violation to `RepoError::DuplicateClientKey` by extended code (measured above), not by matching a message string, and a test proves the mapping fires — and that it does not fire for another constraint — because a message-matching version would pass a naive test and break on a SQLite upgrade.

One consequence is deliberate: **the key is the identity, not the body.** A second create with the same key and a different `project` returns the first session. That is what an idempotency key means, and it is the only behaviour a client can reason about when it cannot tell whether its first request arrived.

## The response is the row plus one key

202, `Content-Type: application/json`, `Location: /api/sessions/<id>`, and a body that is the session object with exactly one extra key, `events`, naming `/api/sessions/<id>/events` — the stream HS-010 will serve and this story only names.

The `Location` header is forward-looking: nothing serves `GET /api/sessions/:id` yet, and the fetchable session URL arrives with HS-013's Sessions screen, the first client that re-reads a session.

The body is built by a `#[derive(Serialize)]` wrapper that flattens `Session` and adds `events`, so the session on the wire is the row and not a hand-built copy of it: a field added to the table appears on the wire when the row gains it, and a field the table does not have cannot appear at all (which is why `events` is the wrapper's and not `Session`'s). `Session` and `Status` gain `Serialize`; `Status` serialises through `as_str`, the same function the `status` column is written with, so the wire word and the column word cannot diverge. `Option` fields serialise as explicit `null` — no `skip_serializing_if` — because one stable shape is easier for a client than a shape that varies by state.

`client_key` therefore appears in the response. Accepted: it is the operator's own key, it is in the row the client is being handed, and hiding it would mean the response is not the row — the very thing this decision exists to prevent.

## Consequences

- A repository failure during a create answers 500 `internal` with a fixed sentence, and the operation and the error go to the log at `error` level. A rusqlite message can contain the database path, so no SQLite text and no path ever reaches a client.
- `RepoError` gains `DuplicateClientKey`; `create` stays the only writer. `tests/session_repository.rs`'s duplicate-key test changes from asserting the SQLite message to asserting the variant, which is what makes the mapping a proof rather than a claim.
- The `Mutex` is this story's, and HS-009 removes it. Until then, a handler that holds the guard across an `.await` does not compile, so the stopgap cannot decay silently.
- Validation lives in `src/http/sessions.rs` with the handler, and `src/sessions/` learns no HTTP status code. No SQL leaves `sessions/repository.rs`; the new `live_count` is a repository method for that reason.
- No provisioning happens here (HS-011), no eviction (HS-020), no event stream (HS-010), no list endpoint (HS-013), and no new guard: this route is a non-`GET`, so ADR-0010's three headers already apply to it and the layer already covers it.
- The cap's answer is temporary by design. HS-020 owns the sentence that replaces it, and this ADR is the record that the 409 was a split, not a shortcut.
- `unknown_field` cannot say which field was unknown, and that is a recorded cost of ADR-0008's `&'static str` message, not an oversight.
- An unbufferable body and a body over axum's 2 MiB default both answer 422 `bad_body`: the route takes `Result<Bytes, BytesRejection>` and maps every rejection, so no extractor failure escapes the envelope. No limit is set or claimed as enforced.
