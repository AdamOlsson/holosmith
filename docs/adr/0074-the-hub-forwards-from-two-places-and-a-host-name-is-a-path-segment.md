# The hub forwards from two places, and a host name is a path segment

This is where one origin starts reaching more than one machine. Item 3 says the hub's **preview** listener forwards
`/<host>/<id>/*` "to that machine's preview listener over the Tailnet, including WebSocket upgrades", and that local
sessions use the name `local`. Item 16's second bullet says preview routing for non-hub machines "is in item 3".
`Roles` says a hub runs "request and preview forwarding", that "API paths are unchanged on every role: a hub serves
`/api/hosts*`, a server serves the session API", and that a hub-only machine creates no sessions.

None of that says how an API call names the machine that owns the session, what the hub sends a peer, how the two
responses that never end (SSE, and a WebSocket through the preview tunnel) are carried, what a peer's *preview* origin
is, or what the five-second and sixty-second forwarding bounds are bounds *on*. This ADR decides eleven things, and
records what a machine with no tailnet could not measure.

## D1 — the preview listener stays a byte tunnel; the API path is decoded and re-framed

Two listeners, two mechanisms, and the split is `ADR-0051`'s own:

| Path | Mechanism | Why |
| --- | --- | --- |
| **preview** (`/<host>/<id>/*`) | the existing `tunnel::pump_both`, bytes both ways | what it carries is not ours to understand: chunked bodies, SSE, `101`. A tunnel cannot be wrong about any of them |
| **session API** (`/api/hosts/<name>/<rest>`) | parse the peer's reply head, relay the body through `Body::from_stream` | it *is* ours: the answer must travel through `axum`, because the envelope (ADR-0008), the four guards and the one `.layer(` (ADR-0010) are the control listener's, and a listener cannot hand a socket to a router |

ADR-0051's "never decode and re-encode" is a rule about a tunnel that does not know what it carries. Here the hub **is
a client of one named peer**: it writes one request, reads one reply head, and copies the status and headers. The
decoding is one direction only (the peer's framing is decoded; `hyper` frames the answer downstream), so there is no
re-encode to get wrong — and `Content-Length` bodies need no decoder at all.

## D2 — `HTTP/1.0` was the cheap answer and it does not work (measured)

Asking a peer for a 1.0 answer would make its server close-delimit every body, which is exactly what a relay wants.
Hyper's server will not close-delimit an **endless** one, so it sends no head at all: `curl -0` against
`GET /api/sessions/<id>/events` on this build printed **zero bytes in four seconds**, while the same request over 1.1
answered headers at once. Hence a streaming chunked decoder (`forward::{Decode, BodyStream}`), not a 1.0 request.

| Arm | Verdict |
| --- | --- |
| **a hand-written streaming decoder** (chosen) | `keyproxy::read_chunked` already decodes chunked by hand, and ADR-0071 hand-wrote an AEAD rather than add a crate; one direction only, pinned by unit tests |
| hyper's `client` feature | ADR-0051 measured its cost (`want`) and refused it; the decoder is a well-specified transform this build already has |
| `HTTP/1.0` and relay until close | measured: the peer never sends a head for a stream |

The reply parser **skips `1xx`** and the forwarded head **strips `Expect`**: `100 Continue` is not a hop-by-hop
header, and a parser that trusted its absence would be wrong the first time a peer sent one. A truncated body is an
**error**, never a clean end (D7).

**The fallback trigger, named so the next agent does not re-litigate it:** if `tests/forward.rs` ever cannot relay a
real peer's SSE event-for-event, the arm is hyper's `client` plus `want`.

## D3 — a forwarded API call is `/api/hosts/<name>/<rest>` → the peer's `/api/<rest>`

The spec gives the hub the `/api/hosts*` prefix and says API paths are unchanged on every role, so this is the one
spelling that satisfies both: **one wildcard route**, the peer's path stays its own, and the caller names the owner
from the registry it already read.

| Arm | Verdict |
| --- | --- |
| **`/api/hosts/<name>/…`** (chosen) | the spec's own prefix for a hub; the peer's path unchanged; one route instead of a second registration of every session route |
| `/api/hosts/<name>/api/sessions…` | the same thing spelled with `api` twice |
| a header naming the host (`X-Holosmith-Host`) | invisible in an address bar, and a caller-supplied value is what ADR-0016 distrusts |
| `?host=` on every route | the same router change with worse ergonomics |
| the **session row's** `host`, resolved across machines | a lookup across machines on every request, and a migration for a value the URL already carries |

Five registrations of that one path, not one `any` route: `Routes` records a method and a kind per entry (ADR-0010),
and a forwarded `GET` cannot change state *at this machine* — so `get` is `ReadOnly` and the four mutating verbs are
`Mutating`, and the manifest stays honest without inventing a method. The method that travels is the client's.

**`/api/hosts/local/<rest>` is a `308`** to `/api/<rest>`: bodyless, path-relative, decided before the row lookup (no
row may be named `local`), and **not** ADR-0008's envelope, because a redirect is not a refusal. It is what gives a
client one path shape — `/api/hosts/<host>/…` for every host, this machine's included — without the hub dialling
itself. `/api/hosts/local` with no rest is the fallback's `404 not_found`: one path per fact. A forwarded path whose
first segment is `hosts` is `403 not_forwarded`, because the registry is each machine's own and a relayed registration
would be a second-hand claim about a third machine: **one hop is the whole promise, and this is what makes it
structural.**

## D4 — the hub authors the request it forwards

`Host` and `Origin` become the **peer's** own origin (its guard compares `Origin` as text against its `control_origin`
and `Host` against its `allowed_hosts`), `X-Holosmith-Client: 1` says this machine is a Holosmith, and three headers
are the caller's and travel **verbatim**: `Content-Type` (what the body is), `Idempotency-Key` (which retry this is —
a create replayed through a hub must answer the peer's *row*, not a second session) and `Last-Event-ID` (where a
stream resumes). Hop-by-hop headers, the caller's `Host`/`Origin`, `Expect` and `tailscale-user-login` do not travel;
`Connection: close` ends the exchange and `Content-Length` is recomputed from the buffered body.

The forwarding route is **not** guard-exempt, so a forwarded non-`GET` is first checked against the **hub's** origin
and client header, and only then re-authored. The rewrite is not "the hub trusts the caller's origin"; it is "the hub
is the author of the call it makes".

**Consequence, stated rather than discovered:** a peer cannot tell a forwarded call from a call at its own origin.
That is exactly the hole **/V11** closes (`allowed_hubs` and the whois), and the shape it has to verify is
`forward::request_head`. Two facts follow from the guard's own text and are prerequisites rather than checks:

- **the row's origin must be the exact `control_origin` the machine reports**, ports included, because the guard
  compares the header as text while `hosts::is_local` folds ports. Another spelling is `403 bad_origin` at the peer —
  the gate's `case_forward` measures that (a `wrongspelling` row naming the peer's `localhost` while its origin is
  `127.0.0.1`), and it doubles as "a peer's own refusal survives the trip";
- `tailscale-user-login` is dropped, so the peer's `owner` check is vacuous for a forwarded call (its
  `(None, _) => true` arm). That is the to change, not this story's.

## D5 — a machine's preview origin is measured, never configured

`hosts` gains one nullable column, `preview` (migration `0007`, `SCHEMA_VERSION` 7), filled by the probe from the
peer's **own health answer** — the fifth key, `preview_origin`, which has ridden `/api/health`. Two
reasons, in order of weight:

1. **the upgrade's `Origin` must be the peer's.** Its listener compares the header against the `preview_origin` in its
   own config; a hub that forwarded the browser's origin (the *hub's*) would get `403 preview_foreign_origin`, and so
   would one that sent none. Only the peer can say what its preview origin is — so without this value, acceptance
   bullet 2's "including WebSocket upgrades" is false;
2. **dialling.** A preview listener is a second listener on a second port: deriving `https://<row host>:8444` from
   deploy's `tailscale serve --https=8444` is a guess about a key the peer owns.

`POST /api/hosts` does not accept it (a caller-supplied origin would point a hub anywhere — ADR-0016's rule), it is
published as `Option<String>` in `GET /api/hosts`, and a row no probe has carried one for
refuses a forwarded preview with a sentence naming the probe rather than dialling a guess. **The limitation, recorded
rather than discovered:** the value is refreshed at the merge — boot time — only, so a hub started before its server
keeps `preview = NULL` (and `reach = unreachable`) for the life of the process. A lazy re-probe would be a second
writer to a row the merge owns, on the request path, with `PROBE_TIMEOUT` of latency; it is not this story's, and a
hub restart is the fix until something asks for better.

The probe's **local shortcut** answers `config.preview_origin` in the same `Healthy` value a round trip would: a row
naming this machine is stored without a request, so it must be stored with the same two facts.

## D6 — the bound is on the reply's head, and the slow arms are three

`forward::TIMEOUT` = **5 s** and `forward::SLOW_TIMEOUT` = **60 s** (spec item 3's rule 6, handed here by ADR-0065 D8
and ADR-0066 D12). They cover the dial, the request and the reply's **head** — never the body: item 3's streams have
no natural end (ADR-0018), so a bound that covered the body would truncate an event stream.

The slow arm is chosen by the **method and the path**: the last segment is `merge` or `publish`, **or** the method is
`DELETE` and the path is one session (`sessions/<id>`). The third is the own landmine about rule 6: a
delete asks the peer's agent to stop and waits out its `abort_grace` — ten seconds by default — before it snapshots
the worktree, so a blanket five seconds would answer `504` for a delete the peer is executing.

**Item 3 names two routes and this story gives three.** The delete's arm is a deviation from the letter of the
acceptance, argued from the peer's own worst case: a bound that answered `504` for a delete the peer is executing would
break the bullet it is meant to serve ("the hub forwards API calls"), and the alternative is a five-second lie. It is
recorded here rather than discovered later, and `forward`'s unit test pins all four numbers.

The numbers are **this machine's constants, not the peer's config**: a peer configured with a longer `abort_grace` can
still outlast its arm and get a `504`. That is recorded, not fixed — deriving a bound from a peer's config would make
one request's deadline depend on another machine's file.

A timeout is `504 upstream_timeout`; a peer that cannot be reached is `502 upstream_unreachable`; both sentences name
the machine, its origin and what was measured. Reachability is **not** consulted before dialling: it is a fact about
the last probe, and a machine that was asleep and is now awake deserves the request (`case_forward` measures a row
marked `unreachable` at boot that answers later).

## D7 — a peer that dies mid-stream is a close, then a sentence

The hub relays the body; when the upstream ends before its framing says it is complete, the stream yields an **error**
(`the peer ended the body before its framing did`), `hyper` aborts the answer after the head, and the client's socket
closes. With a kept `Content-Length` that is a truncation the client can see; with a chunked answer it is a missing
terminator — either way the client is not left waiting, and `EventSource`'s reconnect is answered by the next request's
`502` (a non-200 ends its retries).

**No frame is invented inside somebody else's stream.** The hub does not parse an event stream it is relaying, and an
unknown `kind` is a frame `transcript.ts` drops anyway. What the operator gets instead is one `warn` at the hub naming
the host, the session and the bytes moved.

## D8 — `Content-Length` is kept and `Transfer-Encoding` is never copied

The hop-by-hop set does not travel (the answer's framing is this machine's), but the peer's own
`Content-Length` **does**. Three reasons: the decoder's `Framing::Length` makes "exactly N" structural; a bounded
answer is what this API mostly is; and keeping it is what lets a gate prove a forwarded row is the server's row **byte
for byte** with `cmp` rather than a grep through chunk sizes. A peer that dies with k of N sent errors the stream, and
`hyper` aborts the connection — a visible truncation, not a clean end.

The SSE arm is chunked because the peer chunked, and a forwarded bounded answer keeps the peer's own header: the cost
of the choice, said out loud, is that the two framings differ by what the peer chose rather than by one rule of this
machine's.

## D9 — a hub-only machine serves, lists and refuses

`POST /api/sessions` on `role == hub` answers `403 sessions_disabled` **first of the machine's refusals** — before the
drain and before the disk floor, which is the family it belongs to and the fact that makes those two questions
unnecessary. It is *not* before the request's shape: the body's fields and the `Idempotency-Key` header are read
first (`request_parts`), and `create_locked` raises the refusal after the key's own lookup, so a retry keeps the
retry rule. On a hub that has never held a row the difference is invisible; on a machine whose role was changed from
`both` to `hub` with rows behind it, a replay answers its old row rather than `403` — the retry rule winning, which is
the same choice the drain and the floor make. No row is written either way.
`GET /api/hosts` and `GET /api/sessions` answer on every role (the registry is every machine's, item 16), and the
forwarding route exists only where a machine forwards — so a `server` answers `/api/hosts/<name>/…` with the
fallback's `404 not_found`, which is what makes "a server serves the session API" true of its **path space**.

## D10 — no `sessions.host` column, and no new config key

The owning machine is in the URL the caller used: the machine that owns a row calls itself `local`, and every other
machine names it. A `host` key on the row would be `local` on every row of every machine (a column with one value), and
the hub could not rewrite it — it relays bytes, and the answer is not its own to edit. A config key for a peer's preview
port would be a key per host, which is what the registry already is.

`preview::HOST` becomes an alias of `hosts::LOCAL`, so the one word has one spelling.

**The page does not change, and that is a limitation rather than a design.** The embedded app still fetches
`/api/sessions/<id>` and its stream, so a hub's *sessions screen* shows only the hub's own (none, on a hub-only
machine) and opening a remote session needs the host-aware helper — `apiPath(host, rest)`, one function that
returns `/api/<rest>` for `local` and `/api/hosts/<host>/<rest>` otherwise, plus the `host` each row was listed under.
This story's demo is therefore the API (a script) and a browser opened at `/workshop/<id>/` on the hub's preview
listener, which needs no page change at all.

**One rule for a row, even this machine's own.** A `both` machine whose `hosts.toml` names itself (item 16's "one file
copied to every machine" says it will) forwards `/api/hosts/<its own name>/…` over the network into its own process:
correct, wasteful, and not special-cased. `local` is the *word* for "this machine", and a row is a row. The preview
path's `local` short-circuit stays, because that one is the browser's own spelling of the request rather than a
registry row's.

## D11 — the gate stands two machines in with two boots

`case_forward` is two artifacts on one box: a `role = "server"` boot on pinned control and preview ports, and a
`role = "hub"` boot whose `hosts.toml` names the server's control origin. **The API half needs no daemon** — both are
`backend = "stub"`, and a `hub` is asked no Docker question at all — and the preview half needs the daemon and the image
and prints one `SKIP` without them. The seed naming loopback is legal where `POST /api/hosts` would answer
`422 bad_host_url` (ADR-0073 D9): that asymmetry is the case's whole licence.

Two things the harness needed and now has: `start_peer`/`stop_peer` (its own `--admin-dir`, its own `peer_*` globals,
because `start_serve`'s comment says "only one serve is up at a time"), and `forward_fetch <port>` (`fetch` always
talks to `$port`, and a case with two machines has two ports). The mid-stream death is measured as `read -t`'s **1**
(EOF) against **142** (a timeout), because a client that waits is the failure the acceptance names.

## What this machine could not measure

No `tailscaled` and no way to add a 100.64 address, so: a real MagicDNS name, a real tailnet, TLS to a peer, and the
whois adds. The transport's TLS arm (`s_client`) is the shape ADR-0071 and ADR-0073 already measured for their
own children, and `tests/forward.rs` drives the plain arm end to end; the gate's two machines are loopback, which is
also why the seed is what makes them legal. The demo says the same thing in the same words.
