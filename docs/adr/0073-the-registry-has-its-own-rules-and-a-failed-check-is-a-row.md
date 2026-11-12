# The registry has its own rules, and a failed health check is a row

This is the first phase-3 story, and it is where the build stops being one machine. Everything before it assumes
"the machine" is the one you are talking to; this story adds the list of *other* machines, the two routes that write
it, the optional seed file every machine is given, and — the first of its kind in this build — **an outbound request
to another machine**.

Item 4 gives the registration three checks (the URL is http(s), the host a MagicDNS name or an address in
100.64.0.0/10, the port in an allowed range; redirects are not followed; the `/api/health` reply must carry the
Holosmith name and version fields). Item 16 gives the seed its job (each machine reads an optional `hosts.toml` at
startup and merges it into its hosts table, the file winning on conflicts). Neither says what "unreachable" means for
a row, what a delete's "live sessions" are, or which transport the health check uses.

So this ADR decides nine things, records one asymmetry it chose on purpose, and says plainly what a machine with no
tailnet could not measure.

## D1 — the probe is one `s_client` child for `https` and the hand-written plain socket for `http`

Item 4 says the URL may be `http(s)`, so both schemes are probed. `https` is one
`openssl s_client -quiet -connect host:port -servername host -verify_return_error -verify_hostname host` child, and
`http` is the shape `http::health::ask` already has: one `GET /api/health`, `Host`, `Connection: close`, an 8 KiB cap.

| Arm | Verdict |
| --- | --- |
| **`s_client`, ADR-0071's sender, reused in shape** (chosen) | the machinery, its deadline and its landmines already exist; no dependency |
| `http://` only | item 4 says http(s), so refusing `https` fails the acceptance's own clause — and a tailnet host's certificate is the point |
| a TLS/HTTP-client crate | against ADR-0001 and `check-static.sh`'s registry-only rule, for one request |

The child inherits **no `-CAfile`** on purpose: `SSL_CERT_FILE` is how a machine with a private CA configures every
tool, and it is also what makes the gate's stand-in possible (`case_push` owns that trick and its landmines). The
request is written to the child's stdin and the **handle is held** — closing it makes `s_client` send its
close-notify before the response arrives (`keyproxy::forward` measured that) — and `Connection: close` ends the
exchange. The wait is `try_wait` against `PROBE_TIMEOUT` (3 s, the same bound the loopback client puts on the same
endpoint), killing the child at the deadline; only then is its output read.

Its streams are **pipes, not files**: the answer is a health payload a kilobyte at most, far inside a pipe's buffer,
so the child cannot block writing it while this thread waits, and once the child has exited both pipes are at EOF.
`push::send` uses files for the same shape and a different reason — what it must never do is *read* a pipe whose
writer is still running.

## D2 — `http::health::Answer` grows `name`, its fourth field

The struct's own doc gave three fields one reason: `preview_origin` and `application_server_key` are the app shell's
business and `role` is this machine's config, so "a client struct that carried them would be a second reading of a
shape it does not use". `name` was in that list by accident: it is a constant, and nothing read it.

Now something does. The registration is *about* the name ("the `/api/health` reply must carry the Holosmith name and
version fields"), so `hosts::probe` compares it with `health::NAME` and refuses anything else. One reader for one
shape, and every existing payload fixture (`tests/deploy.rs`, `tests/doctor.rs`, `src/doctor.rs`, `verify.sh`'s
`health_responder`) already spelled `"name"`, so nothing moved but the struct.

The body parse is `health::parse_answer`, one function, so the loopback client and the probe cannot disagree about
what a health answer is.

## D3 — the table is one additive migration, and a row's URL is an **origin**

`migrations/0006_hosts.sql`, `SCHEMA_VERSION` 6. Six columns: `name` (primary key), `url`, `reach`, `version`,
`last_error`, `last_checked_at`. Additive, so `docs/schema/previous.sql` stays whatever the last release refreshed it
to (a release step) and `tests/schema_previous.rs` passes untouched.

**`name` is the identity.** A name is what the operator picks and what forwards with (`/<host>/<id>/*`), so
`POST /api/hosts` is an **upsert** on this key: registering a machine twice is the operator retrying, and the seed's
"the file wins on conflicts" is the same statement. Two names may name one URL — an alias is not a conflict, and
resolving it would be inventing which row the operator meant.

**`url` is a `config::Origin`, not a free URL.** `Origin::parse` already refuses a scheme that is not http(s), a
path, a query, a fragment and whitespace — which is the acceptance's first clause for free — and a registry row is an
origin because the probe asks `GET /api/health` at it and nothing else, and will append the path it forwards.
The column is TEXT and the type is the reader's.

**`reach` plus `last_error` is one row's two facts, not a probe history.** Item 16 says "marked unreachable with the
last error, not removed"; a `probe_history` table would be a second table with a retention question nothing asks.
`version` is kept because the probe has to read a Holosmith's version to accept it at all, and throwing it away would
make the next story re-ask.

`reach` is text rather than a `CHECK` constraint, for ADR-0011's reason: a corrupt row is an error at read time that
names the text it found, and adding a word does not need a migration.

## D4 — the host predicate and the port range are constants in the new module

**The predicate is its own function, not `egress::address_problem` inverted.** `egress` refuses 100.64.0.0/10,
loopback, link-local and RFC 1918 *on purpose*: its job is to stop a container reaching the tailnet. A registry row
exists to name it. The two are inverses over the same range, and one implementation with two callers who want
opposite answers is a policy that will be changed by whichever caller reads it next. `hosts::is_tailnet` is two
lines.

Accepted: a **MagicDNS name** — lowercase labels ending `.ts.net`, with at least one label before the suffix, checked
as *text* because a host that is asleep must still be registrable and a machine with no resolver must still validate —
or an **IPv4 literal in 100.64.0.0/10**. Refused with a sentence: loopback, RFC 1918, a public name, an IPv6 literal
(item 4 names one range and a tailnet's IPv6 range is not it).

**The port range is a constant, not a 33rd key.** `hosts::MIN_PORT = 1024` plus each scheme's own default (80/443):
the unprivileged range, plus the port a URL means when it leaves the port out. A rule that refused 443 would make
`https://box.tail1234.ts.net` legal only when spelled without its port, and Tailscale's certificate is for 443.
Item 11's `push::LONG_TURN` is the precedent for a policy the spec names and no key spells; `KNOWN_KEYS` stays 32 and
the golden config does not move.

**`local` is a reserved name** (`422 bad_host_name`), because spells *this* machine's sessions with it. A
registered host called `local` would make every forwarded path ambiguous, and the ambiguity would arrive at the first
request rather than at registration.

## D5 — `hosts.toml` is `[[hosts]]` tables, read by hand and all-or-nothing

```toml
[[hosts]]
name = "workshop"
url = "https://workshop.tail1234.ts.net"
```

Any line that is not blank, a `#` comment, the header, or `name`/`url` inside a table is a refusal naming its line
number — `vapid::parse`'s taste at the size a list needs. The `toml` crate is not a dependency and two keys do not
earn it (ADR-0001). One malformed line refuses the **whole file**, and the merge then stores nothing: a half-read
seed would leave the table holding rows the operator never wrote.

**"The file wins" = the name wins**, because the name is the identity (D3): a file row replaces an API row of the
same name, and a row the file does not mention **survives**. A merge that deleted unmentioned rows would undo an
operator's registration the next time a file predating it was copied in, which is the opposite of item 16's purpose.

## D6 — the seed is the operator's file, so it is not filtered by the host predicate

The file is checked for **shape** — an origin, and a name that is not `local` or malformed — and not for item 4's
host rule or port rule. This is deliberate and it is the ADR's one asymmetry:

- `hosts.toml` is on the machine's own disk, written by the operator, and is as trusted as `config.toml` (which names
  a Docker host, a projects root and a merge target).
- `POST /api/hosts` exists for values that arrive over a network from a browser, which is what the predicate is for.
  the "Add and remove are available with the same validation as `POST /api/hosts`" is about *those* paths, and
  the `hub host add` likewise.

The cost is that an operator's file can point a hub at a URL no API call could create. The hub forwards only to what
its own file says, the file is root-owned on a deployed machine, and the alternative — a seed that cannot express
`http://127.0.0.1:8080` — would make the file unable to describe the one host that is always there. It is also what
lets a machine with no tailnet measure the probe at all (D10).

## D7 — the delete's refusal is about the **local** row, and a later change owns the rest

Item 4's last bullet: "removing a host is refused while it has live sessions, unless forced". A hub owns no sessions;
a **server** does. So:

- A row is **local** when its origin equals this machine's `control_origin`, with each side's own port folded through
  `Origin::with_port` (so `https://box.tail.ts.net` and `https://box.tail.ts.net:443` are one answer). That is the
  only row whose sessions this build can count: `DELETE` refuses it while `live_count()` — `starting`, `idle` or
  `running`, item 8's cap's own set — is above zero, `409 host_busy` with the count, and `?force=true` (the delete's
  existing spelling, `force_requested` reused) removes it anyway.
- A **remote** row's delete is not refused. Its sessions are not this machine's to count, and pretending otherwise
  would mean a `409` with no measurement behind it. the forwarding is what will answer it honestly, and that story's
  Computers screen is what will show the counts.

A **local row is not probed**: asking this process to reach itself over its own origin is a question about the boot
order — the merge runs before the listener serves, so the answer would be "connection refused" — and not about
health. It is stored `reachable` with this build's version, which is what the round trip would say once the process is
up. That shortcut is `probe_one`'s and it does not care which caller asked, so a `POST /api/hosts` whose origin is
this machine's own is answered the same way: `201`, `reachable`, this build's version, no round trip. It is the one
row in the table whose `reach` is a claim rather than a measurement, and the claim is "this process is the thing that
answers that origin".

**A `--port 0` boot cannot recognise the local row.** `apply_overrides` writes the flag into `control_origin` before
the merge runs, and `serve` re-derives the origin's port only after `bind` — which is after the merge. So on such a
boot (a dev run, and the gate's default) a seed row naming this machine is probed like any other and, because the
listener is not serving yet, born `unreachable` on a perfectly healthy machine. A pinned port — every deploy, and
`case_hosts`, which pins one — is unaffected. Moving the merge after `bind` would fix it and would put the probe on
the runtime's own thread or behind a second lock; the wart is documented instead.

## D8 — a probe that cannot reach marks the row; only a non-Holosmith answer refuses

| the probe | `POST /api/hosts` | the seed's merge |
| --- | --- | --- |
| nothing answered — connect, DNS, TLS, timeout | `201`, row `unreachable` **with the sentence** | row `unreachable` with the sentence |
| an answer that is not a health payload — status ≠ 200, no/other `name`, no version, not JSON | `422 not_holosmith`, no row | row `unreachable` with the sentence |
| a health payload named `holosmith` | `201`, row `reachable` with **its** version | the same |
| the row is the local row | `201`, `reachable`, this build's version | the same |

Two bullets that look like they conflict are two sentences of one rule. Item 4's "anything else is rejected with a
distinct code" is a *reply* that is not a health payload — a machine that answered and is not one of ours, so a
registration of it would be a lie. Item 16's "marked unreachable with the last error, not removed" is a machine that
did not answer: asleep, off the tailnet, mid-reboot. A registration of a machine that is merely asleep is the
operator telling the truth in advance.

The seed's merge has no client to refuse, so a non-Holosmith answer there is one more unreachable row whose sentence
says what answered instead — `hosts::accept` and `hosts::merge_outcome` are those two mappings, one function each,
side by side.

## D9 — the merge runs at startup, in `serve`, before the listener binds

Spec item 16 says "at startup". It runs after `Database::open` and before the bind, synchronously: parse, store (one
lock scope), probe each row **with the lock dropped**, then store what the probe found — the probe leaves the machine
and must never hold the database lock (ADR-0012/ADR-0013's rule, one socket further out).

Three failures and their arms:

- a **missing** file is one `warn` and no rows touched — item 16's file is *optional*, and a machine that has not been
  given one must boot;
- a **malformed** file is one `warn` naming the line and no rows touched (D5's all-or-nothing);
- a host that does not answer is a **row**, never a boot failure.

A boot that refused over a hosts file would take the control UI down for a machine whose sessions are fine — the
egress proxy's rule (ADR-0045). The cost is that a boot pays up to `PROBE_TIMEOUT` (3 s, **the whole exchange**) per
unreachable row before it serves, which is the price of item 4's "marked unreachable" being a fact about *this* boot
rather than a timer. Name resolution is the one thing that bound does not cover: `to_socket_addrs` and the child's
own resolver have no timeout of theirs. A periodic probe is not asked for, and is not here.

## D10 — what this machine could not measure, and how the measurement was split instead

No `tailscaled`, no `sudo`, and `ip addr add 100.64.0.1/32 dev lo` answers `Operation not permitted`: **no MagicDNS
name resolves here and no 100.64/10 literal can be reached.** The gate's `case_hosts` therefore stands three TLS
listeners in on `localhost` through a private CA in `SSL_CERT_FILE`, and seeds them — the seed is not filtered by the
predicate (D6), which is what makes a loopback row legal there.

So the gate measures, through the artifact: the four URL rules and two name rules with their own codes; a healthy
**TLS** answer with the host's own version; a `302` that is impossible to follow (the stand-in records exactly one
request); a payload with no `name`; a MagicDNS name that resolves nowhere marked unreachable with its sentence; the
local row's `409`/`?force=true`; and two boots for "the file wins" and "an unmentioned row survives".

What it cannot measure through the artifact is the API's `422 not_holosmith` against a live listener, because the API
will not accept a loopback URL. `tests/hosts.rs` splits that arm where its two halves are: `hosts::probe` against
real listeners (a healthy payload, a `302`, a foreign name, a closed port), and the **mapping** — `hosts::accept` and
`hosts::merge_outcome` with a refused outcome — as functions. Not measured anywhere: a real MagicDNS name, a real
tailnet host, and Tailscale's own certificate.

## Rejected

- **A `hosts` route that forwards** — that story's, and a forwarder inside the registry would put a timeout budget and a
  stream policy in a table's module.
- **`role` and session counts on the row** — the acceptance names them, and a column nothing reads is a promise
  about a shape (the health payload's rule). The `version` column is the exception, because the probe had to read it
  to accept the answer at all.
- **Deleting a row because a probe failed** — item 16 says "not removed", and a machine that is off is the row's most
  useful state.
- **A `probe_history` table** — a retention question nothing asks.
- **Validating the seed with item 4's rules** — D6.
- **A config key for the port range** — D4.
- **`probe::Temp`, a scratch directory for the child** — `push::send` writes the child's streams to files because it
  must not read a pipe whose writer may never end; this answer is bounded and read after the child exits, so pipes are
  the smaller and correct shape (D1).
