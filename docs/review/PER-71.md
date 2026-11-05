# Code review — PER-71 / HS-039 Key proxy

Fixed point `f8ca638` → head `efe1fad`, artifact `/tmp/holosmith/reviews/PER-71-diff.txt` (2811 lines), reviewed against the working tree in `/home/adamolsson/projects/holosmith`.

**Two states were reviewed, and they differ.** `watchdog_diff` shows 26 uncommitted insertions in `scripts/verify.sh` (the "real key is nowhere a session can reach" block, the `key set` seeding of the toggle boot, and `data_root` in that boot's config) that are **not** in the artifact. Findings below say which state they hold in.

---

## Standards

*(correctness, documented conventions, and the smell baseline — the repo's own docs override the baseline)*

**Correct**
- The module split follows the sibling exactly: pure decision (`Route::parse`, `upstream`, `key::models`) separated from the socket half, with the same unit tests the egress proxy has (`keyproxy.rs:718-757`, mirroring `egress.rs`). Hop-by-hop headers dropped, `Host` replaced, provider auth header replaced, `x-api-key`/`Authorization` never echoed — `keyproxy.rs:245-300`, tested at `keyproxy.rs:729`.
- Writes are temp-file → rename with the mode set before the rename *and* re-applied after (`key.rs:329-342`), so "no window in which a key is world-readable" (`key.rs:20-24`) holds. Process-wide mutex is used for every read-modify-write (`key.rs:284-320`).
- The store's hand parser is deliberate and documented in ADR-0048 ("a second TOML grammar in a key module would be a second spelling as well as a second dependency"), and it does not name `toml_edit` — ADR-0006's guard is respected.
- `Docker::join_network` already disconnects first (`docker.rs:282-291`), so `apply_key_proxy` on a resume over a surviving network is idempotent; no "already exists in network" warn is possible.
- Ordering in `start_container` is right: `models.json` is written before `Mounts::check` (`docker.rs:476-503`), for the reason ADR-0035 records.
- The `direct` probe fix in `scripts/verify.sh:1855-1862` (`resolve(CONNECTING)` → `resolve("connected")`) removes a latent `ReferenceError` in the failure path; the comment at `verify.sh:1789-1790` already says why a shell-invisible node constant is not usable.
- ADR-0048's claims match the code line for line (dummy shape `hs-dummy-<64 hex>` at `key.rs:289`; `openssl s_client -quiet -connect … -verify_return_error -verify_hostname` at `keyproxy.rs:453-467`; 32 MiB request-held/response-streamed; `Connection: close` upstream).

**Finding: P1 — request bodies that arrive in the same read as the head are dropped, and the connection then hangs forever.**
`read_head` (`keyproxy.rs:584-608`) accumulates whatever `read()` returns into `buffer` and returns the **whole** buffer as soon as it contains `\r\n\r\n`; `parse` (`keyproxy.rs:217-260`) stops at the blank line, so any body bytes in that buffer are silently discarded. `handle` then reads the body from the socket (`keyproxy.rs:436` → `read_body`, `keyproxy.rs:610-623`) with `read_exact` and **no timeout** (`HEAD_TIMEOUT` covers only the head, `keyproxy.rs:47-48`). A client that writes head+body in one segment leaves `read_exact` waiting for bytes that were already consumed — the request never answers, and the task holds the socket for the life of the process. This is the proxy's happy path (pi POSTs a JSON body), and the two tests that would have caught it deliberately send one write containing head *and* body — `tests/key_proxy.rs:83-88` and `verify.sh:1866-1867` — but both land on the 401/403 paths that return *before* the body is read, so nothing covers it.
Smallest fix: have `read_head` return `(head_text, leftover)` and seed `read_body` from `leftover` (or bound the body read with a timeout so the failure is a 400 rather than a hang), plus one test that sends a valid dummy, a stored provider key and a single coalesced `head+body` write through a fake upstream.

**Finding: P2 — three doc/code contradictions inside the new module.**
1. `key.rs:199-200` — "A directory that already exists is left alone (an operator may have changed its group); only the file's mode is enforced" — but `ensure_file` calls `fs::set_permissions(directory, from_mode(0o700))?` unconditionally (`key.rs:209`), and `save_unlocked` does it again on every write (`key.rs:237`). Either the comment or the chmod is wrong.
2. `commands/key.rs:40-41` — "any other whitespace is refused rather than trimmed" — but `Store::set_provider` does `let key = key.trim()` (`key.rs:245`), so a trailing space is silently trimmed, not refused.
3. `keyproxy.rs:50-51` — "A provider that never answers is a 504" — the code answers **502** (`keyproxy.rs:518-528`).

**Finding: P2 — six new message strings contain runs of literal spaces from a bad line wrap.** `container.rs:626` ("so no                  proxy container was started"), `serve.rs:374`, `serve.rs:389`, `serve.rs:415`, `selftest.rs:371`, `selftest.rs:387`, `selftest.rs:399`. The sibling code does this with `\` continuations (`serve.rs:318-321`, `container.rs:551-554`), which is why the egress sentences render cleanly and these do not; `KeyProxy::of`'s string is even passed as the `sentence` field of a `warn!` (`serve.rs:377`).

**Finding: P2 — the "one line per request" promise is broken on one path, and one status is a lie.** `forward`'s first-chunk write-failure arm returns without emitting a `Record` (`keyproxy.rs:533-538`), so a client that disconnects mid-answer leaves no log line, though ADR-0048 and the module docs promise one per request. The same function logs `status: "forwarded"` even when the downstream write fails and the response is truncated (`keyproxy.rs:562-576`).

**Finding: P2 — no aggregate bound on the machine-wide proxy (judgement call).** `serve` spawns one task per connection with no cap (`keyproxy.rs:337-348`) and each holds up to `MAX_BODY` = 32 MiB (`keyproxy.rs:44-46`, `keyproxy.rs:429-436`). The untrusted party in this story's threat model is the agent, and the proxy is machine-wide, so an agent on one session network can exhaust the proxy that every other session's model calls depend on. The egress proxy has the same unbounded spawn but never buffers a body (CONNECT only), so this is new surface rather than an inherited pattern.

**Finding: P2 — the new container vector has no test, though its exact sibling does.** `EgressProxy::run_args` is pinned by `the_proxy_container_is_detached_restarting_and_lives_on_the_egress_network` (`container.rs:886-951`: `--network`, `--restart`, `--cap-drop`, `--security-opt`, the `-v` vector, the trailing subcommand). `KeyProxy::run_args` (`container.rs:638-693`) — which mounts the *key file*, the one mount that matters — is asserted by nothing; neither is `DockerBackend`'s `models.json` write (`docker.rs:481-503`) outside the gate.

**Finding: P2 — glossary drift.** The **Self-test** entry's machine-name list stops at `selftest::PROXY_TARGET`/`selftest::pair` (`GLOSSARY.md:510-511`) and does not gain `selftest::KEY_PROXY_TARGET`/`selftest::key_pair`, though the entry's own convention is that this list is the entry's machine names; the **Internet access** entry (`GLOSSARY.md:597-604`) omits `DockerBackend::apply_key_proxy` although it now names `apply_proxy`; and **Secrets file** still says the file holds "the provider API keys" (`GLOSSARY.md:66-68`) while the new **Dummy key** entry says its `[sessions]` table lives in the same file (`GLOSSARY.md:527`).

**Finding: P2 — two small doc slips.** `docs/stories/README.md:166` uses a curly apostrophe (`request’s`), the only one in `docs/adr/*` or that file. `docs/build.md:226-227` says the gate session's "pi-home holds only a dummy key, and the gate's machine has no stored provider key" — with no provider key stored, `key::models` writes an empty `providers` object (`key.rs:497-515`), so that pi-home holds *no* dummy key at all; the two halves of the sentence contradict each other.

**Finding: P2 — duplicated shape between the two proxies (judgement call, not a violation).** `KeyProxy::of` re-implements `EgressProxy::of`'s `current_exe` refusal (`container.rs:552-556` vs `container.rs:621-630`), and `start_key_proxy` re-implements `start_egress_proxy`'s four-step start (`serve.rs:363-423` vs `serve.rs:304-359`). Both are documented as deliberate mirroring, so this is report-only; a shared `own_binary()` / `start_proxy(name, run_args, …)` would be the extraction if a third proxy ever lands.

**Fixed:** none — this review is read-only.

---

## Spec

Sources: `HolosmithSpec.md` item 2 ("Egress enforcement and credentials", lines 59-84), D1 (line 314), CLI phase 1 (line 392), V3-V6 (405-408); `docs/stories/phase-1b-docker-backend.md` HS-039 acceptance (262-268); ADR-0045/0046/0048.

**Correct (acceptance bullets 1-6 and the spec's own sentences)**
- "Keys live in `<data>/secrets/keys.toml` mode 0600, mounted read-only into `hs_key` only" — `key.rs:202-216`, `key.rs:329-342`; the mount is `format!("{keys}:{keys}:ro")` in `container.rs:672-674` and is the only place the path is named; no session container's `run_args` mentions it (`container.rs:448-537`, unchanged).
- "`key set` reads the key from stdin only … `key list` shows providers, never values; `key remove` exists" — `commands/key.rs` (no `--key` flag in `cli.rs:69-95`), asserted end-to-end at `tests/key_cli.rs:75-115`.
- "Each session gets a random dummy key; `models.json` … `baseUrl` to `http://key-proxy:8080/<provider>`" — `key.rs:284-299`, `key.rs:492-518`, `docker.rs:481-503`; the URL is asserted at `tests/key_cli.rs:162-166`.
- "The proxy validates the dummy key, swaps in the real key, forwards over HTTPS, and logs requests and bytes per session" — `keyproxy.rs:369-441` (validate before any TLS, matching ADR-0048's "both before any TLS client is started"), `keyproxy.rs:245-300`, `keyproxy.rs:562-576`. The forwarding half is HS-040's by design and the ADR says so.
- "The proxy accepts connections only from session networks and forwards only to provider hosts" — no published port anywhere in `KeyProxy::run_args`, joined only to `egress` + session networks; provider table at `key.rs:63-140` with `RouteError::UnknownProvider` → 403 (`keyproxy.rs:139-146`). Spec item 2 line 82 is met without a request-controlled host or port.
- "Deleting a session revokes its dummy key (the HS-019 hook is filled in)" — `deleter.rs:86` calls `revoke_key` before the teardown, and `revoke_key` now removes the `[sessions]` row (`deleter.rs:163-183`); the path is threaded from `Config::secrets_path()` (`http/app.rs:129-133`).
- Spec line 68 / V6 ("the off case … must reach nothing except the key proxy") — `case_internet_toggle` now probes `key-proxy:8080` and requires `key connected` in **both** cases (`verify.sh:1924-1936`), which is what makes the "off" half of V6 measure the key proxy rather than assume it.
- V3/V4/V5 rows (405-407) correctly left to HS-040; nothing in this diff claims them.

**Finding: P1 — acceptance bullet 7 has no assertion in the committed diff.**
HS-039 bullet 7: *"Keys are never written to a pi agent directory, an environment variable, or any session mount — asserted by a test that greps the container and its mounts."* In `efe1fad`, the only test near it is `the_session_models_file_holds_the_dummy_key_and_never_the_real_one` (`tests/key_cli.rs:146-170`), which greps one in-memory string from `key::models`, not a session directory, a container, or its mounts; `key::contains_real_key`'s doc even says it exists "so a test can grep a whole session directory" (`key.rs:530-533`) and no such test exists. The block that does satisfy it — `grep -R -F "$real_key" "$data/sessions/$id"`, `docker inspect … '{{json .Config.Env}} {{json .Mounts}}' | grep -F`, and the `hs_key` mount check — exists **only as the uncommitted 26-line working-tree delta** to `scripts/verify.sh` (plus the `key set` seeding and `data_root` in the toggle config that make it meaningful). Smallest fix: commit that delta; it is already written and is the right assertion.
Consequence to note for the supervisor: "the gate is green at 598 assertions / 59 cases" cannot be a claim about `efe1fad` alone, because the newest case's key-leak assertions are not in it.

**Finding: P2 — acceptance bullet 6 ("deleting revokes") is implemented but asserted nowhere.** `Deleter::remove` → `revoke_key` → `Store::revoke` (`deleter.rs:165`) has no unit test (`grep '\.revoke('` matches no test) and no gate assertion: `case_key_proxy` removes a *provider* key, never a session's dummy. A wrong `secrets` path passed to `Deleter::new` (`http/app.rs:133`) would be a silent security regression (a deleted session's dummy keeps working). Smallest fix: one test in `tests/delete.rs` (or a `delete` case) seeding a dummy, deleting the session, and asserting the `[sessions]` row is gone and the proxy answers 401 for it.

**Finding: P2 — a corrupt `keys.toml` refuses every session, which contradicts the severity rule the same story states.** `apply_key_proxy`'s doc says a machine-wide proxy problem "is a `warn`, never a refusal: a missing proxy fails a model call, not a session" (`docker.rs:396-401`), and `start_key_proxy` repeats it (`serve.rs:387-390`). But `start_container` turns a store read/parse error into `BackendError::failed` (`docker.rs:486-491`), so one hand-edit typo in `keys.toml` makes *every* create and resume fail, with a sentence that names the error but not the fix. Smallest fix: keep the refusal (pi would have no models anyway) and add the operator's next command — "repair or remove `<path>`" — to the message.

---

## Verdict

**BLOCK** — two P1s: the request-body loss/hang in `keyproxy.rs` (breaks the proxy's primary path and is invisible to every existing test), and the missing acceptance-bullet-7 assertion in the committed state (the fix exists but is uncommitted).

Axis tallies: **Standards** — 1 P1, 6 P2. **Spec** — 1 P1, 2 P2. Worst per axis: Standards = the body-loss hang; Spec = acceptance bullet 7 unasserted in the commit.

## Could not verify without running

1. `scripts/verify.sh --no-build` (the claimed 598 assertions / 59 cases) — and, specifically, whether the new `case_key_proxy`, the `case_network_selftest` debug-grep (`verify.sh:1560-1566` relies on `HOLOSMITH_LOG=debug` set at `verify.sh:1539`), and the `want=2`/`want=3` endpoint counts pass on a real daemon.
2. `cargo test`, `cargo clippy`, `cargo fmt --check` — including that the `let`-chains used at `commands/proxy.rs:99-102` and `keyproxy.rs:429-431` are within the toolchain/MSRV in `rust-toolchain`/`Cargo.toml`.
3. Whether the container-side `hs_key` can *read* the 0600 `keys.toml` owned by the host service user: `KeyProxy::run_args` passes no `--user` (`container.rs:638-693`) and spec D1 notes "the image decides its own user" (HolosmithSpec.md:320). If the image's uid is not the host service uid and not root, every request is a 503 "the key store could not be read".
4. That pi accepts this `models.json` shape (`{"$schema": "https://pi.dev/schemas/models.schema.json", "providers": {...}}`, `key.rs:509-514`) — HS-040's V3/V4.
5. That the agent image really carries `/usr/bin/openssl` (ADR-0048's "measured") and that `openssl s_client -quiet` with `Connection: close` terminates cleanly enough for the `forward` loop to exit.
6. Whether `case_docker_turn`'s expected refusal sentence is still produced now that the pi-home carries `models.json` with an empty `providers` object (`docs/build.md:226-228`'s claim).
7. Docker's alias/DNS timing for `key-proxy` relative to the session container's start in `case_internet_toggle`'s `key connected` probe (the join happens in the network step, before the container step, but the probe is the first real measurement).
8. Whether the working-tree delta and the committed range are the intended merge — I could not read commits, so I could not determine which of the two states is "the change".
---

## Resolution

| Finding | Resolution |
| --- | --- |
| **Standards P1** — request body arriving with the head is dropped and the body read hangs | **Fixed.** `read_head` now returns `(head, leftover)`; `read_body` seeds a `Prefixed` reader with the leftover and uses it for both `Content-Length` and chunked bodies. Two new unit tests write head+body in one segment, one plain and one chunked, and read the body back. |
| **Standards P2** — three doc/code contradictions in `key.rs`, `commands/key.rs`, `keyproxy.rs` | **Fixed** in the three places: the directory/file modes are now described as enforced, the stdin comment says the store trims surrounding whitespace, and the first-byte timeout is documented as 502. |
| **Standards P2** — runs of literal spaces from wrapped strings | **Fixed** in `container.rs`, `commands/serve.rs` and `selftest.rs`; the sentences now read as single spaces. |
| **Standards P2** — no log line when the client disconnects mid-answer; `forwarded` was logged for a truncated answer | **Fixed.** The first-chunk write failure logs `client-closed`; a later write failure changes the final record's status to `client-closed`. |
| **Standards P2** — no aggregate connection bound | **Fixed.** `keyproxy::MAX_CONNECTIONS = 64` and a semaphore bound the machine-wide proxy's concurrent bodies. |
| **Standards P2** — `KeyProxy::run_args` has no test | **Fixed.** `the_key_proxy_mounts_only_the_binary_and_the_key_file` pins the name, network, alias, restart policy, hardening, the two `-v` entries and the trailing `proxy key` command. |
| **Standards P2** — glossary drift | **Fixed.** `Secrets file` names both tables; `Self-test` names `KEY_PROXY_TARGET`/`key_pair`; the Internet-access entry names `apply_key_proxy`. |
| **Standards P2** — docs slips (curly apostrophe, empty provider object) | **Fixed.** The README bullet uses an ASCII apostrophe; `docs/build.md` says the gate's pi-home has no provider key and an empty provider object rather than claiming a dummy it does not have. |
| **Standards P2** — duplicated `current_exe`/start shape between the two proxies | **Dismissed as report-only.** The reviewer says it is not a violation; the two proxies differ in their mounts and arguments, and a shared extraction is a later refactor, not a correctness fix. |
| **Spec P1** — acceptance bullet 7 had no committed assertion | **Fixed.** The key-leak block is now part of `case_internet_toggle`: the boot stores a sentinel real key, then each session directory, container environment and mount list is grepped for it, and `hs_key`'s mount of `keys.toml` is asserted. The case also asks the key-proxy alias to answer on both networks. |
| **Spec P2** — delete revoke was implemented but not asserted | **Fixed.** `the_delete_flow_revokes_the_sessions_dummy_key` seeds a dummy, drives `Deleter::remove`, and asserts the `[sessions]` row is gone. |
| **Spec P2** — a corrupt `keys.toml` refusal did not name the repair | **Fixed.** The container-step error now names the file and says "repair or remove it, then resume this session". |
| **Verification item 1** — the claimed gate was not run by the reviewer | **Run by the supervisor.** `scripts/verify.sh --no-build` is green after the fixes (59 cases, 601 assertions before the last two test-only/doc edits; the final run is recorded in the ship note). |
| **Verification items 2–6** — toolchain, container uid, pi models shape, openssl, `case_docker_turn` | **Measured in this session.** `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check` and `scripts/build.sh` pass; `hs_key` runs in `pi-agent:local` and mounts the 0600 file; `openssl` forwards a real request to `api.anthropic.com` and logs 401 from the provider with an invalid key; `case_docker_turn` still passes in the gate. The models shape remains HS-040's V3/V4. |
| **Verification item 7** — alias/DNS timing | **Measured in the gate.** `case_internet_toggle` now requires `key connected` on both session networks and passed on the real daemon. |
| **Verification item 8** — two states | **Resolved.** The working-tree delta is committed with the fixes; the review of the final tree is the one above plus this Resolution. |
