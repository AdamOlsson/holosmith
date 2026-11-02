# Review — PER-37 (HS-005)

**Fixed point:** `fe5ea6c` (HEAD before this work), reviewed against the **uncommitted** tree plus
its four untracked files. In-scope files read in full: `crates/holosmith/src/http/{app,guard,error,mod}.rs`,
`src/commands/serve.rs`, `tests/{guards,http_contract}.rs`, `tests/support/mod.rs`, `docs/build.md`,
`docs/adr/0010-browser-facing-guards-are-one-layer.md`, `GLOSSARY.md`, plus `src/config/mod.rs`
(read-only: `Origin`, `allowed_hosts`, `is_host_name`), `HolosmithSpec.md:93–104,310`, the HS-005
story slice and the approved plan. `Cargo.toml`/`Cargo.lock` are unchanged.

**Verification basis.** This role has no shell, so it did not execute the gates; it read the diff
artifact and the tree. The supervisor ran, after sourcing the toolchain env: `cargo fmt --check`;
`cargo clippy --all-targets -- -D warnings`; `cargo test` (133 lib + 20 config_cli + 12 db_migrations
+ 39 guards + 11 http_contract + 2 pin_single_source + 12 serve_cli + 4 version_cli, all green);
`scripts/build.sh` (both musl targets, static, the app served from an empty root natively and under
qemu); and the six-case demo plus a CORS-leak audit, all green — before the fixes below and again
after them.

**Verdict:** Standards OK with notes, Spec OK with notes, no blocker and no should-fix. Every finding
was a nit and all were fixed.


Method: read the diff artifact `/tmp/holosmith/reviews/PER-37-diff.txt` in full, then re-read the working tree at `fe5ea6c`+changes (`src/http/{app,guard,error,mod}.rs`, `commands/serve.rs`, `tests/{guards,http_contract,support/mod}.rs`, `docs/build.md`, `docs/adr/0010-*`, `GLOSSARY.md`), plus `config/mod.rs` (Origin, allowed_hosts, is_host_name), `HolosmithSpec.md:93–104,310`, the HS-005 story, the plan, and ADR-0002/0007/0008. No shell, so no gate re-run; nothing edited.

## Standards

Invariants checked and holding (no finding): one `.layer(` in `crates/` (`http/app.rs:213`, pinned by `tests/guards.rs:790`), `axum::serve` once (`serve.rs:74`), `into_router` `pub(crate)` (`app.rs:239`); `is_guard_exempt` still the only exemption list and only GET/HEAD `/api/health` (`app.rs:310-311`); `config/mod.rs`, `tests/serve_cli.rs`, `Cargo.toml`, `Cargo.lock`, the banner and every `http_contract.rs` assertion untouched; guards are built *after* the ephemeral-origin fix-up (`serve.rs:113-117`), so `--port 0` cannot ship mismatched guards; no pi version spelled in the new docs; no test pins a message sentence.

- **nit** — `src/http/guard.rs:126` / `src/http/app.rs:291`: `strip_cors` takes no `self` and touches nothing on `Guards`, yet it is an associated function called as `Guards::strip_cors(&mut response)`. A reader looks for a method on the value and finds a free function in disguise. Smallest fix: `pub fn strip_cors(&mut Response)` at module level in `guard.rs` and call `guard::strip_cors(...)`.
- **nit** — `tests/guards.rs:443-494`: `no_answer_carries_a_cors_header` is driven by two parallel 9-element arrays (`answers` at :451, `requests` at :464) zipped by index. `zip` truncates silently, so a tenth case added to either array is silently not run; and the codes spelled out in the comments (`bad_host`, `bad_origin`, …) are never asserted, only the status. Smallest fix: one array of `(method, path, expected_status, &[(name, value)])` and assert `body_code` on the 403 rows.
- **nit** — `tests/guards.rs:61-63`: `raw()` is a one-line pass-through to `support::request` (Middle Man, used only by `send`). Inline it.
- **nit** — `tests/support/mod.rs:37-38`: the move made `stop` and `handle` `pub` (they were private fields of the old `http_contract.rs` copy) and nothing outside the module reads them, so a future test can bypass `stop()`/`finish()` and the drain assertions. Smallest fix: drop both `pub`s.
- **nit** — `src/http/app.rs:37,44,67-160,164` + `tests/guards.rs:729-735`: the method→kind mapping lives twice — a literal per `register` call site, then re-derived by `match entry.method` in the test — and `ManifestEntry.method` is a `&'static str` standing in for `http::Method` (primitive obsession; string comparison in the test instead of `Method::GET`). Smallest fix: store `Method` and derive `kind` from it in one place.
- **nit** — vocabulary/export drift: `RouteKind` and `ManifestEntry` are new public domain names, but "manifest" has no `GLOSSARY.md` entry (the diff adds only `**Guard**` at `GLOSSARY.md:243`) while `PROMPT.md:10` makes the glossary the vocabulary authority; and `http/mod.rs:9-10` re-exports every other test-facing type but not these two (`guards.rs:20` reaches in via `http::app::RouteKind`). Smallest fix: one glossary entry plus the two names in the `pub use`, or make both module-private.
- **nit** — `docs/stories/README.md:117`: "Nine decisions were made … Each has an ADR" — ADR-0010 makes ten and has no bullet, whereas ADR-0006 and ADR-0009 each got one (and that file was edited by PER-34 and PER-36). Either add a bullet and the new count, or state that the list is frozen at slicing time because 0010 was taken during implementation.

**Standards verdict: OK with notes.** No blocker or should-fix; the code is idiomatic for this repo and every invariant the brief named holds. All findings are message/mapping/export housekeeping plus one stale count.

## Spec

- **nit (ADR, not behaviour)** — `docs/adr/0010-…:44`: two miscounts in one bullet. "`Routes` gains **five** method helpers" — six were added (`app.rs:67,76,90,104,118,132`: get/head/post/put/patch/delete). "**Three** registrations move to the new spelling — the health route, `app.rs`'s own unit test, and the `/slow` and `/hang` fixtures" — that is four, and the diff shows exactly four. Fix the two numerals.
- **nit (ADR text only; the behaviour is the accepted finding, not re-reported)** — `docs/adr/0010-…:45`: "`OPTIONS /api/health` changes from `405` plus `Allow` to `403 bad_client`" reads as though `Allow` is gone. It is not: `tests/guards.rs:517-521` measures that axum attaches the route's `Allow` outside the layer. The test comment is the accurate record; the normative ADR should say so too, or a later reader will "fix" it with the second layer ADR-0007 forbids. Smallest fix: append "… to `403 bad_client`, which still carries the route's `Allow` header, attached outside this layer."
- **nit — the one item-4 sentence the ADR never records** — `HolosmithSpec.md:101` says "When behind `tailscale serve`, **require** the `Tailscale-User-Login` header to equal the configured owner"; `src/http/guard.rs:114-116` treats an absent header as passing (`(None, _) => true`), so any request that does not traverse the proxy skips the owner check outright. The story box narrows the sentence to "requests **carrying** `Tailscale-User-Login`" and `tests/guards.rs:573` (`an_absent_login_is_allowed`) records the decision — but ADR-0010, whose opening promises to record "where a literal reading of item 4 would have produced something the operator could not use", is silent on the largest such case. Smallest fix: one bullet in ADR-0010's §Consequences: absent login is accepted because the server cannot tell whether it is behind the proxy; the `Host` allow-list is what bounds that.
- **nit — box 6's evidence cannot fail** — `tests/guards.rs:707` (`no_get_route_is_mutating`) and `:723` only assert that the entries produced by `Routes::get`/`head` carry the label `ReadOnly`, i.e. the constructor's own table; a handler registered with `.get()` that writes still passes, and the manifest has one GET row (`/api/health`). `docs/adr/0010-…` ("the types are the enforcement") is true of the label, not of the behaviour. The box's literal text is met, so this is a note, not a gap: fix by adding one sentence to that ADR section saying a mutating `GET` is prevented by the registration vocabulary plus review of the handler, not by this test — otherwise the next author reads the passing test as proof.

**Spec verdict: OK with notes.** All seven acceptance boxes are implemented and each has a test that would fail if the guard were removed; the guard order, the four codes, the CORS strip on both branches, the unbypassable single layer, and the demo path (guarded POST → 404 envelope) all match item 4 and the ADR. The findings are ADR accuracy and one unrecorded deviation, no behaviour change requested.

**Merge verdict: OK with notes** — no blocker, no should-fix; nothing for the supervisor to run beyond the gates already reported green.
## Resolution

Every finding was triaged. There were no blockers and no should-fix items; all eleven nits were
accepted and fixed. Two findings turned out to be wrong on their own premises and are recorded as
such rather than silently "fixed".

| Finding | Status | How / why |
| --- | --- | --- |
| **S1** `strip_cors` is a free function wearing an associated function's clothes | **fixed** | It is now a module-level `pub fn strip_cors(&mut Response)` in `http/guard.rs`, called as `guard::strip_cors(...)` from the layer. It reads no `Guards` state, so it no longer pretends to. |
| **S2** the CORS sweep is driven by two parallel arrays that `zip` truncates silently, and the 403 rows never assert their `code` | **fixed, and it caught a real bug** | One nine-row array of `(label, method, path, headers, status, Option<code>)`; status asserted on every row, `code` asserted wherever it is expected, plus `cors_free` on every row. **The new assertion immediately failed on a row the old test had been passing**: the case labelled `bad_origin` sent a *correct* `Origin`, so it had actually been answering `bad_client` and only the status was ever checked. That row is now a `POST` with no `Origin`, and all four 403 codes plus the preflight's are asserted. This is the finding paying for itself. |
| **S3** `raw()` is a Middle Man | **fixed** | Deleted; `send` and the missing-`Host` test call `request` directly. |
| **S4** the move made `Served::stop`/`handle` `pub`, so a test could bypass `stop()`/`finish()` | **fixed, by a different route than the review proposed — its premise was false** | The claim "nothing outside the module reads them" was wrong: `tests/http_contract.rs` awaited the `handle` field directly inside the drain-deadline test. Both fields are private again and a `pub async fn join(self)` was added (with the same panic message), so `finish` is `stop(); join()`, `stop()` stays a method, and the deadline test's `timeout(5s, served.handle)` became `timeout(5s, served.join())`. **No assertion changed** — the assertion text in that file is byte-identical to `fe5ea6c`; only line numbers moved. |
| **S5** `ManifestEntry.method` is a `&'static str` (primitive obsession) and the test re-derives the kind by string | **fixed for the type; the "derive kind from method" half is dismissed with a reason** | `method` is now `axum::http::Method` and the test compares `Method::GET`/`Method::POST`. Kind is **not** derived from the method: deriving it would make `no_get_route_is_mutating` a tautology and leave box 6 with no evidence at all, whereas an explicitly declared kind is a declaration the test can contradict. ADR-0010 now says plainly that the types enforce the label and review of the handler body is what stops a writing `GET`. |
| **S6** `RouteKind`/`ManifestEntry` are new public names with no glossary entry, and are not re-exported | **split: re-export fixed, glossary dismissed** | Both are re-exported from `http/mod.rs` and the test imports them through `http::`, like `App`/`Routes`/`RequestCounter`. The glossary entry is dismissed: `GLOSSARY.md` is the system's domain language (Machine, Config, Session, Guard), and `App`, `Routes`, `RequestCounter`, `RouteKind` and `ManifestEntry` are public but deliberately unglossed internal seams — a route manifest is test evidence, not a domain concept. The rationale is recorded in ADR-0010 so it is not re-opened. |
| **S7** `docs/stories/README.md` still says "Nine decisions … Each has an ADR" | **fixed** | Now "Ten", with the ADR-0010 bullet after ADR-0009. That file was edited by PER-34, PER-35 and PER-36 for exactly this reason, so it is in scope for the story that adds the tenth ADR. |
| **P1** ADR-0010 miscounts: "five" method helpers, "three" registrations | **fixed** | Six helpers and four registrations. |
| **P2** ADR-0010 reads as though `OPTIONS` loses its `Allow` header | **fixed** | It now says the `403 bad_client` still carries the route's `Allow`, attached by axum outside this layer, and that removing it would need the second layer ADR-0007 forbids. |
| **P3** ADR-0010 never records the absent-`Tailscale-User-Login` deviation | **fixed** | New consequences bullet: an absent login is accepted because the server cannot tell whether it is behind the proxy, and the `Host` allow-list is what bounds direct access. This is the largest place a literal reading of item 4 would have broken the operator, so the ADR now says so. |
| **P4** ADR-0010 overstates what box 6's test proves | **fixed** | The section now says the types enforce the label, not the behaviour: a mutating `GET` is prevented by the registration vocabulary plus review of the handler body, and the manifest test is a tripwire for a mislabelling helper. |
| *(discovered)* the review's own premise for S4 was false | **recorded** | See S4. A dismissal would have been wrong too — the privacy fix was worth making — so it was fixed by adding the accessor the finding implied rather than by dropping the finding. |

After the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (133
lib, 20 config_cli, 12 db_migrations, 39 guards, 11 http_contract, 2 pin_single_source, 12
serve_cli, 4 version_cli — all green) and `scripts/build.sh` (both musl targets, static, the app
served from an empty root natively and under qemu) are green, `tests/serve_cli.rs`, `src/config/`,
`Cargo.toml`, `Cargo.lock` and every `tests/http_contract.rs` assertion are unchanged, and the
six-case demo plus the CORS-leak audit still hold.
