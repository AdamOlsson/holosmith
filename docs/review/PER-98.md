# Review — PER-98 / HS-066 (Web Push backend)

Fixed point: `b87ff16`. Reviewed as `b87ff16...HEAD` (33 files) by a read-only reviewer running the
`code-review` skill, with the spec axis sourced from `HolosmithSpec.md` item 11, the story slice and the
acceptance bullets. Saved verbatim; the `## Resolution` table below is the orchestrator's.

## Review

Reviewed `b87ff16...HEAD` via `/tmp/holosmith/reviews/PER-98-diff.txt` (33 files) plus the real files where the diff was not enough. Two axes reported separately, per the skill.

---

## Standards

### Correct (evidence)

- **Repository owns its table, and the walk proves it.** `push/repository.rs:113-160` is the only file with a `push_subscriptions` statement; `every_push_subscriptions_statement_is_in_this_file` walks `src/**` *and* asserts the walk found >40 files before asserting no offenders (`repository.rs:196-210`) — exactly the rule in `docs/notes.md:493-495`.
- **Pure table + seams.** `Watcher::observe(&mut self, event, now)` is pure and takes the clock (`push/mod.rs:140`); the threshold, the program (`Tools.openssl`), and the sender (`Deliver`) are three separate seams (`push/mod.rs:114-120`, `:262-263`, `:196-199`) — the `Compactor::with_window` / `quota::DU` precedent.
- **The envelope.** Both extractors are the fallible shapes (`Result<Bytes, BytesRejection>`, `RawQuery`) so no axum `text/plain` refusal escapes (`http/push.rs:41`, `:89`; `http/app.rs:770-786`), and the guard covers both routes because the kind is `Mutating` (`http/app.rs:761-787`, `tests/push.rs:340-364`).
- **No `unsafe`, no new dependency** (`Cargo.toml` is not in the diff; `images::sha256` is reused rather than re-written, `images.rs:327-331`).
- **Capability URLs never logged.** Every `push` line names `host`/`audience` only (`push/mod.rs:381-401`, `:478-481`); `Target::of`'s refusals name shapes not values (`send.rs:296-303`).
- **Key material handling is right.** `Temp` is `0700`, files `0600` set at `open` rather than chmod (`jwt.rs:390-424`), removed on `Drop` (`:426-430`), signing input on stdin so no scalar is in argv (`jwt.rs:283-294`), and `s_client`'s stderr is drained on its own thread (`send.rs:168-187`) — the hazard `docs/notes.md:1870-1872` records.
- **The crypto is what its vectors say.** I checked each primitive against its document: `multiply`'s MSB-first bit loop and `R` at the top of the word (`crypto.rs:196-209`) match SP 800-38D §6.3; `ghash`'s length block is `len(A)=0 ‖ len(C)` in bits (`:213-225`); `seal`'s `J0 = nonce‖0x00000001`, keystream from `inc32(J0)`, tag `GHASH(C) ^ E(J0)` (`:239-251`) is §7.1; `expand` implements `SubWord(RotWord(w))^Rcon` (`:139-158`); `shift_rows` is row `r` left by `r` (`:178-186`); `mix_columns` is FIPS 197 §5.1.3 (`:161-172`). Vectors spot-checked against the documents they name: RFC 4231 §4.2/§4.3, RFC 5869 §A.1/§A.3, FIPS 197 C.1 (`69c4e0d8…`), and SP 800-38D's two 96-bit-IV cases (`58e2fcce…`, `0388dace…ab6e47d4…`) are the published values. `open` (`:256-268`) is genuinely `seal`'s inverse and is `cfg(test)`-only.
- **RFC 8291 is the RFC's four steps** in the RFC's order (`encrypt.rs:139-183`), the `0x02` delimiter is appended by `seal` itself rather than by a caller (`:179-183`), the header is `salt(16)‖rs(4)‖0x41‖point` (`:186-193`), and the §5 golden test drives the *shipped* functions — `derive` → `keys` → `body` → `crypto::seal` — and names all five intermediates the RFC publishes (`:236-330`). Steps that could silently be wrong (wrong `key_info`, missing `0x02`, `rs`/`idlen` misplacement) all change the 144-byte body, so the test would fail.
- **The disposition table and the identity rules** are right and tested at the far end (`send.rs:100-118`, `tests/push.rs:711-738`, `scripts/verify.sh:4464-4500`).

### Finding: P1 — the TLS child has no deadline, so one unresponsive endpoint stops the whole notification channel for the process's life

`send.rs:191-204` reads the child's stdout in a loop until EOF, and only then kills it (`:205-206`). Nothing bounds `write_all` (`:160`) or the read loop, and `announce` awaits that `spawn_blocking` with no timeout (`push/mod.rs:352`). An endpoint that accepts the connection and then neither answers nor closes — a middlebox, a stale NAT mapping, or a service that ignores `Connection: close` and keeps the socket open after its `201` — leaves `s_client` blocked in `read` forever, so `Kill`'s guard is never reached and `run()`'s frame loop never resumes: **every later notification for every session is lost**, silently, with no log line (the code that logs is downstream of the hang). That the sender is "off the writer's path" is true; that "a push service that answers slowly [cannot] stall **a request**" is also true — but nothing bounds the task itself.

This contradicts the build's own documented rule that every child/connection on a background path carries a deadline: `container::PROXY_START_TIMEOUT` with "everything on the startup path is bounded" (`docs/notes.md:1205-1207`), `keyproxy::FIRST_BYTE_TIMEOUT = 120 s` (`keyproxy.rs:55-56`), `egress::CONNECT_TIMEOUT`/`HEAD_TIMEOUT` (`egress.rs:56,64`), `health::ASK_TIMEOUT` (`health.rs:19`), `selftest::PROBE_TIMEOUT`, `ladder::bounded`, and `build_bounded`'s "polls `try_wait` to the bound, kills on expiry" (`docs/notes.md:1555-1557`).

*Smallest fix:* follow `build_bounded`'s measured shape — send the child's stdout **and** stderr to files under the existing per-send `Temp`, poll `child.try_wait()` to a `DELIVER_TIMEOUT` constant (60–120 s, the `keyproxy` value), kill on expiry, then read the head from the file and report `Kept` with a `warn`. That also removes the hand-rolled stderr-drain thread.

### Finding: P2 — `doctor`'s `systemd` doc comment now heads `openssl`, and `fn systemd` has none

`doctor.rs:258-269` is `systemd`'s doc block ("Whether this machine has **systemd**… refusing here would make `doctor` unrunnable on the machine that runs no systemd and serves perfectly well") immediately followed, with no separating `///` line, by `openssl`'s own first line — so the first four lines of `openssl`'s doc describe a different function, and `fn systemd` (`doctor.rs:333`) is undocumented. The hunk inserted `openssl` between the two. *Smallest fix:* move those four lines back above `fn systemd` and add a blank `///` between the two blocks.

### Finding: P2 — five sites cite ADR-0071 decisions by the *plan's* numbers, which the ADR does not have

`docs/adr/0071-…md` has **D1–D11** only. The code cites `D12` and `D13`:
- `doctor.rs:265` and `preflight.rs:99` — "ADR-0071's D12" (the doctor finding is **D9**);
- `push/mod.rs:31`, `http/app.rs:253`, `push/send.rs:34` — "ADR-0071's D13" (**D10**, "no config key").

D12/D13 are `/tmp/holosmith/plans/PER-98.md`'s numbering. The ADR is the canonical record (`docs/notes.md:782-806`; the repo treats a false sibling-ADR citation as a finding — cf. `docs/review/PER-36.md:102-108`). *Smallest fix:* renumber those five to D9/D10. Related, same family: ADR-0071 D10 names the constant `JWT_EXP`, the code has `jwt::EXP_SECONDS` (`jwt.rs:41`).

### Finding: P2 — `Pusher::threshold()` is dead, and its doc says `serve` uses it

`push/mod.rs:270-273`: "The threshold this pusher measures a long turn against, **for the `debug` line `serve` prints**." `serve.rs:412` prints `crate::push::LONG_TURN.as_secs()` instead, and nothing else calls `threshold()` (no caller in `src/` or `tests/`). The story therefore has two answers to one question, and one of them is a doc comment that is false. *Smallest fix:* `let pusher = state.pusher();` … `long_turn_s = pusher.threshold().as_secs()` … `let task = pusher.spawn();` — or delete the method and its doc.

### Finding: P2 — a `Lagged` frame leaves a stale start behind, which can announce a turn that was *short*

`push/mod.rs:290-293` warns and carries on without touching `watcher.started`. The module's own promise is "A turn whose `turn.start` this task did not see … has no duration, so `turn_finished` cannot fire for it: **recorded, not guessed**" (`push/mod.rs:24-27`). Under lag that becomes guessing: if `idle` for turn 1 and the `turn.start` of turn 2 are both dropped, the remaining recorded start is turn 1's, and turn 2's `idle` is measured from it — a `turn_finished` for a turn that ran under 20 s, i.e. spec item 11's negative inverted. Lag is reachable, not theoretical: the buffer is 1024 events (`events/writer.rs:23`) and `announce` awaits every child before reading the next frame. *Smallest fix:* on `RecvError::Lagged`, `self.watcher.forget_all()` (clear `started`) — a lag means at least one ending was missed, so no recorded duration is trustworthy — and say so in the module doc.

### Finding: P2 — `status_of`'s six-arm match duplicates `Status::as_str`, contradicting its own comment

`push/mod.rs:189-201`: `Status::as_str(self) -> &'static str` (`sessions/mod.rs:41`), so `find(...)`'s result already answers `&'static str`; the `match to { Status::Starting => Status::Starting.as_str(), … }` is a second spelling of the same table, and the comment above it ("so the table below compares the words the column holds rather than a second copy of them") claims the opposite of what the arms do (baseline smell: *Duplicated Code* / *Repeated Switch*). *Smallest fix:* `let to = to.as_str();` and delete the six arms.

### Finding: P2 — the long-key HMAC arm is measured only by "does not panic"

`crypto.rs:361-362`: `assert_ne!(long, [0_u8; 32])`. It catches the naive `padded[..key.len()]` panic, but it does not verify the *output* for the arm the comment says it is about ("a key longer than the block is hashed first, RFC 2104 §2"). RFC 4231 §4.6 publishes the vector: key = 131 × `0xaa`, data `"Test Using Larger Than Block-Size Key - Hash Key First"`, HMAC = `60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54`. *Smallest fix:* assert that string, as the two cases above it do.

### Finding: P2 — the "payload is three keys" unit test asserts a *copy* of the shipped struct

`encrypt.rs:362-367` declares a local `Payload { host, session_id, kind }` and asserts its key set (`:404-410`) — but the shipped payload is `push::Payload` (`push/mod.rs:458-463`), which the test never touches. Add a fourth field to the shipped struct and `encrypt.rs`'s test, `tests/push.rs` (which cannot read the sealed body), and `push/mod.rs` all stay green; only `case_push` (`verify.sh:4446-4448`) would go red, and it SKIPs without `node`/`openssl`/`python3`. So in `cargo test` alone, spec item 11's "only `{host, session_id, kind}`" is unmeasured. Since `push::encrypt::tests` is a descendant of `push`, the shipped type is already reachable. *Smallest fix:* delete the local struct and build `crate::push::Payload { … }` from the test.

### Finding: P2 — the new `Check::Openssl` is unmeasured on both arms, and the tests point it at the real binary

ADR-0071 D9 records two claims: "a `Pass` with the version line and a `Fail` when it cannot run", and "**`--full` only**, and that is a decision". Nothing tests either. `grep Openssl` finds no assertion anywhere; the `--quick` list and tally (`tests/doctor.rs:259-296`) are pinned and unchanged, and no test pins the `--full` list — so moving `doctor.rs:267-272` out of the `if depth == Depth::Full` block, or deleting the check, would leave the suite green. Worse for the *fail* arm's portability: the ADR justifies the `Tools` field as "the seam a test can own a program with", yet `tests/doctor.rs:97,112,326` and `tests/deploy.rs:144` hard-wire `PathBuf::from("openssl")`, and `full_adds_the_self_test_the_proxies_and_the_image` (`tests/doctor.rs:729-746`) asserts `!report.failed()` — a `--full` report that now *fails* on a machine with no `openssl`, which `docs/build.md:729` explicitly calls supported ("A machine without it still serves and still streams every turn"). *Smallest fix:* an `openssl_shim(dir, version)` beside `df_shim`/`tailscale_shim`, two tests (a `--full` pass naming the version; a shim that exits non-zero is a `Fail` whose sentence names the program), and one assertion that a `--quick` run does **not** contain `"openssl"` among the check words — which is D9's decision.

### Finding: P2 (judgement) — `Disposition::keeps_row` exists only for tests

`send.rs:110-113`; every caller is a test (`send.rs:382-391`, `tests/push.rs:806-810`). Production branches on the variant directly (`push/mod.rs:364-380`). Baseline smell: *Speculative Generality*. Report-only — but if it stays, it is the table test that should assert `matches!(…, Gone)`, the rule production actually runs.

### Finding: P2 (report-only) — `endpoint_of` splits the query on a raw `&`

`http/push.rs:180-193` walks `split('&')` pairs, so an endpoint containing a literal `&` truncates and the `DELETE` answers `404` for a row that exists. A conforming client percent-encodes (`%26`) and this is then correct — the test at `:270-286` covers `%2F` and `?` but not `&`. No real service's endpoint is known to contain one, so this is a note rather than a defect; if it is ever tightened, splitting only on the first `endpoint=` and taking the remainder is the shape `?force=true`'s neighbour uses.

---

## Spec

### Bullet by bullet

1. **Two routes over `push_subscriptions` (`endpoint` PK, `p256dh`, `auth`, `created_at`)** — ✔. Table matches the spec's DDL byte for byte (`HolosmithSpec.md:287-292` vs `migrations/0005_push_subscriptions.sql:23-27`); upsert keeps the first `created_at` (`repository.rs:76-92`, `tests/push.rs:204-225`); `DELETE` removes, a second is `404` (`tests/push.rs:228-247`); manifest kind asserted (`tests/push.rs:340-364`); schema 5 recorded in all three migration tests.
2. **VAPID signs every send; `404`/`410` removed** — ✔, and measured rather than claimed: the token is verified by a **second** `openssl dgst -verify` child against the pair a real `ecparam` made (`tests/push.rs:757-800`) and again by node's `crypto.verify` at the far end (`verify.sh:4196-4212`); the disposition table is one table with the row's survival as the assertion (`tests/push.rs:711-738`), and `403` keeping the row is proven through the artifact (`verify.sh:4490-4510`).
3. **Three triggers** — ✔ all four kind words, with a real stub turn for `turn_finished`, the transitioner's own `unexpected_exit` for `error`, and the ladder's rung 2 for both reason words (`tests/push.rs:517-676`), plus the negatives (under-threshold, no `turn.start`, an unnamed reason, `stopped` with no reason). I traced the ending ladders for double-fire: rung 1 → `abort_completed` with the reason (`turn.rs:343-357`) is one frame; rung 2 → `abort_ladder` then `resume`, and the late `AbortCompleted` is refused by the table (`sessions/state.rs:217-224`), so one turn yields one notification either way. Nothing fires for a session whose turn never started.
4. **No send while a stream is open** — ✔, read at the moment of sending (`push/mod.rs:310-320`), proved both in `tests/push.rs:683-703` (real registry, held/dropped) and across a whole turn with the socket left open (`verify.sh:4390-4432`).
5. **Only `{host, session_id, kind}`** — ✔ in production by construction (`push/mod.rs:458-463`) and end to end by the gate's decrypted key set (`verify.sh:4446-4448`) — **but see the Standards finding on `encrypt.rs:362-367`**: the `cargo test` half of this bullet measures a copy of the struct.
6. **The server, not the containers** — ✔. Sender is the host process (`push/mod.rs:262-263a`, `SslClient`), and `tests/push.rs:840-877` asserts no `docker run` argument and no `--env` name carries `push`/`vapid`/`subscribe` or the endpoint, with a row really stored.

### Scope

No creep. Everything in the diff is in the plan's Files table: the six `push` files, two routes, the two codes, `Database::push()`, the `serve` spawn/abort, the doctor finding, `images::sha256` and `changes::decode` visibility changes, `sessions::RepoError::sqlite` visibility, the migration, `case_push`, and the five docs. Nothing from the non-goals (PWA, service worker, tap-to-open, retries, a `GET`, a config key, `previous.sql`) appears.

### Notes (not findings)

- Item 11's "VAPID keys generated at first start" is satisfied by `server deploy`, not by `serve` — ADR-0058 D1 already rules that "'at first start' therefore means the first `server deploy` of this installation", and the pusher's one-`warn`-and-no-sends for an undeployed machine (`push/mod.rs:418-447`) follows it. Inherited, not this story's.
- `session.status → error` fires for `provisioning_failed` too, i.e. a notification for a session whose turn never ran. That is the literal spec sentence ("`session.status` error"), so it is right, but it is the one arm an operator may find surprising.

---

## Verdict

- **Spec: pass.** All six acceptance bullets are implemented, and each is measured at the level the bullet needs — with the single exception that bullet 5's `cargo test` coverage measures a copy of the shipped struct (P2, one-line fix).
- **Standards: one P1, eight P2.** The P1 is real and should be settled before this ships: `send.rs:191-206` gives the `s_client` child no deadline, so one endpoint that does not close silences every notification for the process's life, silently, against the build's own "everything on a background path is bounded" rule. Everything else is documentation accuracy (the `systemd` doc, the D12/D13 citations, the dead `threshold()`), a sturdier lag arm, and four tests whose claims are weaker than they read.
- Nothing here is P0: no data loss, no capability URL in a log, no broken crypto, no spec inversion in the shipped trigger table.

Tests the supervisor should run (I did not run anything): `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and `scripts/verify.sh` (or at least `case_push` plus `case_server_deploy`/the doctor cases, which are the ones the `Tools` field touches).

## Resolution

| # | Finding | Verdict |
| --- | --- | --- |
| 1 | **P1** — the TLS child has no deadline: one endpoint that never closes silences every notification for the process's life | **Fixed.** `send::DELIVER_TIMEOUT` (60 s, item 12's hub timeout and the key proxy's own order of magnitude) and `POLL` (50 ms). The child's stdout and stderr go to **files** in the send's own `0700` directory instead of pipes this thread reads, and the wait is `try_wait` against one deadline — the shape `docker::bounded` and the image build already use — so the hand-rolled stderr-drain thread is gone too. On expiry the child is killed, the row is **kept** and the caller logs a `warn`: an endpoint that goes quiet is a machine problem, not proof that the browser is gone (`Disposition`'s rule one step out). `Deliver::deliver` takes the directory, so a test's recorder ignores it. The write is not bounded and does not need to be: a request is a few hundred octets, far inside a pipe's buffer, where the read was unbounded by construction. |
| 2 | **P2** — `doctor`'s `systemd` doc block headed `openssl`, and `fn systemd` had none | **Fixed.** The four lines are back above `fn systemd`; `fn openssl` starts at its own first line. |
| 3 | **P2** — five sites cited ADR-0071 as `D12`/`D13`, which the ADR does not have (it has D1–D11) | **Fixed.** `D12` → `D9` (`doctor.rs`, `preflight.rs`), `D13` → `D10` (`push/mod.rs`, `http/app.rs`, `push/send.rs`); the ADR's `JWT_EXP` is now `jwt::EXP_SECONDS`, the name the code has. The plan's numbering is not the record's. |
| 4 | **P2** — `Pusher::threshold()` was dead, and its doc claimed `serve` used it | **Fixed** by making the doc true: `serve` builds `state.pusher()`, prints `notification.threshold()`, then spawns it — one answer to one question. |
| 5 | **P2** — a `Lagged` frame left a stale start behind, so a *short* turn could be announced as a long one | **Fixed.** `Watcher::forget_all()` on `Lagged`, with the reasoning in its doc: a lag means at least one ending was missed, so no recorded start is trustworthy, and forgetting turns a lag into silence — the honest outcome — until the next `turn.start`. This is the arm the module doc's "recorded, not guessed" promised and the code did not keep. |
| 6 | **P2** — `status_of`'s six-arm match re-spelled `Status::as_str` | **Fixed.** `Status::ALL.find(…).as_str()` already answers `&'static str`; the match is gone and the comment now says what the code does. |
| 7 | **P2** — the long-key HMAC arm was measured only by "does not panic" | **Fixed.** RFC 4231 §4.6's vector (`60e43159…0ee37f54`) is asserted, so the arm the comment is about is the arm the test proves. |
| 8 | **P2** — the "payload is three keys" unit test asserted a *copy* of the shipped struct | **Fixed.** The test builds `crate::push::Payload` (a child module sees it), so a fourth field is a compile error there, not a green test. |
| 9 | **P2** — `Check::Openssl` was unmeasured on both arms, and the doctor fixtures hard-wired the host's binary | **Fixed.** `openssl_shim` beside `git_shim`/`df_shim`, used by all three fixtures; `full_reports_openssl_and_quick_does_not_ask` (a `--full` pass naming the version, and `--quick` not asking at all — D9's decision, measured) and `openssl_that_cannot_run_is_a_failure_that_names_the_program`. `tests/deploy.rs` keeps the **real** `openssl` on purpose: the DER walk there is a claim about the machine's own `ecparam`. |
| 10 | **P2 (judgement)** — `Disposition::keeps_row` existed only for tests | **Fixed** by deleting it: production branches on the variant, so both tests now assert the comparison production runs (`!matches!(…, Gone)`), which is the rule rather than a helper one step away from it. |
| 11 | **P2 (report-only)** — `endpoint_of` splits the query on a raw `&` | **Fixed as a test, not as a change.** RFC 3986 makes percent-encoding the client's obligation and the route honours it; what was missing was the measurement, so the delete test now stores an endpoint containing `&` and `?` and deletes it through `encode`, which proves `%26`/`%3F` decode into the row's key. Taking "the remainder after `endpoint=`" instead would swallow a second parameter the day one is added. |
| 12 | Note — item 11's "VAPID keys generated at first start" is satisfied by `server deploy`, not `serve` | **Accepted, inherited.** ADR-0058 D1 already reads "at first start" as the installation's first `server deploy`, and the pusher answers one `warn` and no sends for a machine that never deployed — which is what an undeployed machine should do. |
| 13 | Note — `session.status → error` also fires for `provisioning_failed`, i.e. a session whose turn never ran | **Accepted.** Item 11 says "`session.status` error", and narrowing it to a turn's error would be a condition the spec does not state. |
