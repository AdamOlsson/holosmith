# The push is signed by `openssl` and encrypted in-crate

HS-066 is spec item 11, whose three middle bullets are the whole of it: "Endpoints: `POST /api/push/subscribe`
and `DELETE /api/push/subscribe`, table `push_subscriptions`", "Triggers: a turn finished after running longer
than 20 seconds; `session.status` error; a budget or timeout stop. Not sent while an SSE stream for that session is
open", and "Payload carries only `{host, session_id, kind}`, never content. Tapping opens that session." Its last
bullet is the shape of the machine: "The server, not the containers, needs outbound access to the push services."
The table, the routes and the trigger are ordinary work.
The hard part is that this build has **no HTTP client and no crypto dependency**, and a Web Push message is a
VAPID-signed JWT plus an RFC 8291 `aes128gcm` record. Six things here were open, and the first two are the ones a
later story would otherwise re-open.

## D1 — the encryption is in-crate, because the two alternatives were measured and both fail

`openssl enc` cannot do AEAD on this host: `enc -aes-128-gcm` answers **"AEAD ciphers not supported"** (OpenSSL
3.0.13). `openssl kdf … HKDF` with its `hexkey`/`hexsalt`/`hexinfo` parameters answers the **wrong bytes** for RFC
5869 case 1 — the CLI's hex parameters are not what they look like — so the key schedule cannot be a child either.
That leaves two arms: nine children per message (`pkeyutl` for ECDH, `enc -aes-128-ctr` for the record and a
hand-written GHASH anyway), or the four primitives in Rust. In-crate wins because every primitive is pinned by a
published vector and none of them needs a process: HMAC-SHA-256 (RFC 4231), HKDF (RFC 5869 cases 1–3), AES-128's
encryption half (FIPS 197 Appendix C.1) and GCM's GHASH (two NIST SP 800-38D cases with a 96-bit IV), composed
exactly as RFC 8291 §3.4 composes them against **the RFC's own §5 example, byte for byte** — 144 octets, and the
RFC's `Content-Length: 145` is an erratum this test pins by not repeating.

Two things make that the smaller risk here rather than a shortcut. The plaintext is `{host, session_id, kind}` and
never a message's content (item 11), so the confidentiality at stake is a session's existence rather than a
transcript; and every function is small enough to check by eye — the S-box is the only table a *value* indexes,
which is what AES is, and `AES-128-GCM`'s GF(2^128) multiply is the bit-at-a-time version from §6.3. `SHA-256` is
**not** written a second time: `images::sha256` is this crate's FIPS 180-4 implementation and becomes `pub(crate)`.
Encryption only, wherever an inverse can be avoided: `push::crypto::open` exists for tests (round trip, block
boundary, tampered tag) and is compiled under `cfg(test)`.

## D2 — the two children are `openssl`, and the trust store is the process's own

ES256 over the JWT is `openssl dgst -sha256 -sign <sec1.der> -keyform DER`: the machine's pair is base64url, so it is
materialised as the 121-octet SEC1 DER that `vapid::from_der` parses — the writer and the reader are one encoding,
and a test round-trips `from_der(sec1(pair)) == pair` to prove it — inside a `0600` file in a `0700` per-send
directory under `<data>/push/tmp/`, removed on drop. The DER `SEQUENCE{INTEGER r, INTEGER s}` that comes back is
walked to the 64-octet `r‖s` JOSE wants: each integer left-padded to 32, its sign padding dropped, and anything
longer refused rather than truncated. Hand-rolled P-256 is the ~400 lines where a bug is a `403` that looks like a
config mistake, and `openssl` is already a host dependency.

The send is one `openssl s_client -quiet -connect <host>:<port> -servername <host> -verify_return_error
-verify_hostname <host>` child, with the request head and body on stdin and the response read to EOF; `Connection:
close` makes the service end the exchange, so the child exits. **No `-CAfile`**: the child inherits the process's
trust configuration, so a machine with a private CA sets `SSL_CERT_FILE` exactly as it would for any other tool —
which is also what makes `case_push` possible at all. Four children per message (`ecparam`, `pkeyutl`, `dgst`,
`s_client`), and the salt is **not** a fifth: it comes from `/dev/urandom`, because a sixteen-octet child would be
the one child whose output is not a measurement. The endpoint's URL is never logged: it is a capability URL, so
every line names its **host**.

## D3 — the trigger table reads the event stream, and it is one pure function

`AppState::pusher()` builds a `Pusher` holding an `EventStream` from `events.subscribe()`; `commands/serve.rs`
spawns it after the reconcile, beside the four policies, and aborts it with them. It reads the stream rather than the
row or the turn runner for two reasons: a poll of a column cannot see the difference between one long turn and two
short ones, and the runner is the *policy's* peer, not its subordinate — reading the log is how a second observer
joins without becoming a second writer.

The table is `Watcher::observe(&mut self, event, now)`, one pure function with `now` as a parameter (ADR-0011's rule,
so a test's clock is its own):

| frame | kind |
| --- | --- |
| `session.status` to `error` | `error` |
| to `idle`/`stopped`, reason `turn_timeout` | `turn_timeout` |
| to `idle`/`stopped`, reason `budget_exceeded` | `budget_exceeded` |
| to `idle`, reason absent, elapsed ≥ `LONG_TURN` | `turn_finished` |

`LONG_TURN` is twenty seconds and a **constant**, not a key: item 11 names the number and names no config key
(ADR-0025's rule for the sweeper's tick). A `turn.start` records the arrival instant; **a turn whose `turn.start`
this task did not see — a restart mid-turn — has no duration, so `turn_finished` cannot fire for it**, and every
other transition forgets a recorded start, so a later `idle` is never measured from a previous turn's arrival. The
other stop reasons the build records (`forced_abort`, `idle_stop`, `evict`, `drain`, `disk_quota`, `deleted`,
`expire`, `pi_exit`) are **not** triggers: item 11 names three things and this table is exhaustive. `Lagged` on the
broadcast is a `warn` and a missed notification, never a stall.

## D4 — the stream rule is read at the moment of sending

`announce` asks `StreamRegistry::count(session) > 0` **now**, not at the trigger: a client that connected during the
turn is watching, and suppressing a notification it does not need is the whole point. The state's own registry is
what makes that the real one — the `Pusher` carries the `AppState`, so the count is this process's streams and not a
registry a task happened to be built with.

The rest of `announce` is the discipline every policy in this build keeps: the rows are read under one lock scope and
the guard is dropped, the machine's pair is read next (absent or unreadable is **one** `warn` and no sends, because a
machine that never deployed has no pair and that must not be an error per notification), and then each subscription
is built and delivered on `spawn_blocking`. No database lock spans a `spawn_blocking` or a child.

## D5 — the `sub` claim amends ADR-0058's "HS-066 decides it"

ADR-0058 left the file without one because "a JWT's `sub` claim is not key material… the consumer that signs with
this pair decides it". Decided: `mailto:<config owner>` — the operator's Tailscale login, the same identity the
ownership guard already trusts — and `mailto:holosmith@localhost` when the config names no owner, which is
`snapshot::IDENTITY`'s precedent for a local identity rather than an empty claim a service may refuse. `exp` is
twelve hours (`EXP_SECONDS`), inside RFC 8292's 24-hour ceiling: longer is a leak that keeps working, shorter expires
mid-turn. `aud` is the endpoint's **origin** — `https://host[:port]` and nothing else — because that is the one value
a push service compares.

## D6 — `host` is GLOSSARY's **Local**, and the payload is three keys

`host` is `preview::HOST` (`"local"`, GLOSSARY's **Local**: "the host name a server uses for its own sessions"), so
HS-069's forwarder and HS-074's cross-machine push inherit one spelling rather than a second vocabulary. `kind` is
one of the four words above — three of which the build already uses for the same fact, so a client reads one
vocabulary: `error` is the event kind item 11's second clause names, and the two stop reasons are the words
`turn.rs` already writes on the row. The payload is a `#[derive(Serialize)]` struct with exactly three fields, and
`encrypt.rs`'s unit test seals it, opens it with the user agent's half and asserts the key set is exactly
`{host, session_id, kind}` — so the promise is structural rather than intended.

## D7 — the disposition table, and why `401`/`403` must not delete a row

`200`/`201`/`202` → **Delivered**; `404`/`410` → **Gone**, and the row is removed (item 11's "dead endpoints are
removed"); everything else — including `401`, `403`, `429` and every `5xx` — → **Kept**, with a `warn`. A `401`/`403`
is a **key** problem: a rotated pair, a `sub` the service dislikes, a clock past `exp`. The endpoint is fine and the
browser is still subscribed, so deleting the row would mean an operator's phone silently stops receiving
notifications because this machine rotated a key. Only a service saying the endpoint itself is gone earns a removal,
and a child that could not run is `Err` and keeps the row for the same reason.

## D8 — one additive migration, and the endpoint is the primary key

`0005_push_subscriptions.sql` adds one table, `endpoint TEXT PRIMARY KEY, p256dh, auth, created_at`, and
`SCHEMA_VERSION` becomes 5. The endpoint is the key because that is the identifier a push service mints: a browser
produces one per subscription, so a second `POST` of the same endpoint is that browser re-subscribing — the keys move
and `created_at` does not. A synthetic id would let one browser register twice and receive every notification twice.

`p256dh` and `auth` are rows and not files under `secrets/`, unlike `keys.toml` and `vapid.toml`: those two are the
machine's identity, written once by a deploy and mounted nowhere, while a subscription arrives over HTTP at any
moment, there is one per browser, and a phone that re-subscribes overwrites its own row. And the migration is
**additive** (ADR-0047): an added table touches nothing the previous release reads, so `tests/schema_previous.rs`'s
subset rule is unaffected and `docs/schema/previous.sql` stays a release step.

## D9 — `openssl` is now a dependency of a notification, so `doctor --full` says so

One new finding, `Check::Openssl`, running `openssl version` through `preflight::Tools::openssl` (the seam a test can
own a program with), a `Pass` with the version line and a `Fail` when it cannot run. It appears in **`--full` only**,
and that is a decision: `--quick` is the unit's `ExecStartPre`, so a check there refuses a boot, and a missing
notification channel is not a machine that cannot run a session — the SSE stream still reports every turn.

## D10 — no config key

Item 11 names none, and the three numbers it does not name are constants like the sweeper's tick and the cap's
interval: `LONG_TURN` (20 s), `send::TTL` (one hour — a notification about a turn an hour ago is about a session
somebody has already looked at) and `JWT_EXP` (12 h). `AppState::pusher()` answers `Pusher` and not `Option<Pusher>`,
unlike `sweeper()` and `quota()`, whose keys have a `0`-means-off.

## D11 — what the gate can and cannot measure

`case_push` is daemon-free and stands a real TLS receiver in front of the sender: `node:https` on `localhost`,
verifying the token's ES256 signature against the pair **the case generated** (`crypto.verify` with a `P-256` JWK),
decrypting the body with the subscription's own private key, and appending one JSON line per request so the
assertions read what arrived. It measures the request, the store, the stream-open pair and the disposition table, and
the one trigger the stub can reach from HTTP — the token budget, whose `budget_exceeded` is item 11's fourth named
trigger.

It cannot reach the other two: the stub's turn is a fixed 2.4-second script with no failure mode reachable over
HTTP, and `LONG_TURN` is twenty seconds with no key to shorten it. Those are `tests/push.rs`'s — a real stub turn
paced past a fifty-millisecond threshold for `turn_finished`, the transitioner's own `unexpected_exit` row for
`error`, and the ladder's rung 2 for the two reason words. What no test here measures is a real push service, a real
browser subscription, iOS's installed-to-home-screen requirement and the tap that opens a session: the receiver
stands in for the first two, and the last two are HS-067's.
