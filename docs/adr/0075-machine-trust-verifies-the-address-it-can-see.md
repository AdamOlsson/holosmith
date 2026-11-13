# Machine trust verifies the address it can see

This is V11. Item 4's last bullet is two rules, and the ADR ends by naming the second as its own hole:

> Machine to machine: a machine accepts forwarded calls only from peers listed in `allowed_hubs`, verified with the
> Tailscale LocalAPI whois of the remote address. Browser-facing checks apply at the hub.

`allowed_hubs` has existed and nothing read it, and `forward::request_head` authors a call whose `Host` and
`Origin` are the peer's own and which drops `tailscale-user-login` — so a peer cannot tell a forwarded call from a call
at its own origin, and its `owner` check is vacuous for one. Two hops reach a peer, the session API and the preview, so
the check has to exist once and be asked by both. This ADR decides nine things and records what a machine with no
tailnet could not measure.

## D1 — one `admit`, asked at the edge of each listener

`trust::Trust::admit(peer) -> Result<Admitted, Refusal>` is the whole decision, and it is **sync**: it may run a child.
Both listeners run it on the blocking pool and each spells the refusal in its own words.

| Listener | Where the check runs | Where the address comes from |
| --- | --- | --- |
| control | `http::app::edge`, immediately **after** the four browser checks and only where they apply | `ConnectInfo<SocketAddr>`, so `serve::run` now serves `Router::into_make_service_with_connect_info` |
| preview | `preview::handle`, after `parse_head` and **before** `Route::strip` | `TcpStream::peer_addr` |

Three arms were weighed:

| Arm | Verdict |
| --- | --- |
| **one function, two call sites** (chosen) | the two hops must agree, and the words they answer with must not: one decision, two spellings, exactly ADR-0051's split |
| inside `forward::Link` | the transport does not know which listener is speaking; the preview hop uses it too, and its refusal is a sentence rather than an envelope |
| at the router only | a preview connection is not a request: the listener is a byte tunnel with no router (`ADR-0051`) |

The health endpoint keeps its exemption (ADR-0010's one entry), and that is load-bearing rather than convenient: a
hub's registry is filled by `GET /api/health`, so a machine could never register the hub it is about to trust.

`ConnectInfo` is why a router driven in-process verifies nobody: an address the listener could not report is
`peer_unverifiable` when a list exists, never a pass. `tests/trust.rs` and the unit table both assert it.

## D2 — the identity is the node's short machine name, from one `whois` child

`tailscale whois --json <ip>` is one child (`Tools::tailscale`, default the name on `PATH`), its answer read after it
exits, `try_wait` against a 3 s deadline — `hosts::probe`'s shape, for its reason: a whois answer is a kilobyte and
fits a pipe, so pipes are right here and the streaming shape ADR-0071 needed for a body is not.

The compared identity is `Node.Name` folded to its **first label**, ASCII-lowercased (`hub.tail1a2b.ts.net` → `hub`),
against the `allowed_hubs` entries (validated already by the config as bare host names).

| Arm | Verdict |
| --- | --- |
| **the node's short name** (chosen) | the list is a list of *machines*; the registry keys rows by that name, `hosts::LOCAL` reserves the one word, and the `hub host add <name>` will collect exactly this |
| `UserProfile.Login` | the login is the **owner** and is identical on every node the operator has, so comparing it would admit the stray node this story exists to refuse |
| an address in 100.64/10 | an address changes when a machine rejoins; the config's own validator refuses one as an entry, and the *point* is not to trust an address but to ask who it is |

A name this parser invented would be a name the list might match, so an answer that is not that shape is `None` and
the caller refuses.

## D3 — three arms, and the one that admits without asking

| `allowed_hubs` | peer | answer |
| --- | --- | --- |
| empty | anything | `Admitted::Unchecked` — the operator chose tailnet ACLs (V11's "If it fails" cell) |
| non-empty | `127.0.0.1` / `::1` | `Admitted::FrontDoor` — no child at all |
| non-empty | anything else, whois names a listed node | `Admitted::Node` |
| non-empty | anything else, whois names another node | `Refusal::NotListed` |
| non-empty | anything else, whois fails or names no node | `Refusal::Unverifiable` |

**The front door is `127.0.0.1` and `::1` exactly, not `IpAddr::is_loopback()`.** `tailscale serve` — deploy step 6 —
proxies both origins to loopback, so every browser call, every `holosmith` CLI call, `doctor`, a host probe and the gate
arrive from `127.0.0.1`; asking the tailnet about an address that is not in the tailnet would refuse every browser the
machine has. The rest of `127/8` is an address no deployment uses, so a peer there is a peer like any other — which is
also the only way one box can measure the refusal at all (D7).

**And that same sentence is why this check cannot bite on a deployed machine's forwarded call** — the one arrival
`tailscale serve` hides with the rest. D10 records it rather than leaving it to be discovered.

| Arm | Verdict |
| --- | --- |
| **empty list = ACLs only, non-empty list = verify and fail closed** (chosen) | the default config keeps working, a list is the operator asking for the check, and the fallback the spec names is a *setting* rather than a broken daemon |
| refuse every forwarded call when `tailscaled` is absent | a machine that lists hubs and has no daemon refuses every call — the same outcome as the chosen rule, reached by making the empty list impossible instead of by documenting it |
| accept when the whois fails, and log `unknown` | an unread identity is not an identity; the drain family's warn-and-carry-on is right for a measurement nobody's security rests on, and this one is the whole story |

## D4 — one sentence, two spellings, and never the configuration

| Listener | Status | Code |
| --- | --- | --- |
| control | `403` | `peer_not_allowed` |
| control | `403` | `peer_unverifiable` |
| preview | `403` | `preview_peer_not_allowed` |
| preview | `403` | `preview_peer_unverifiable` |

The sentence is `trust::Refusal`'s, and the control listener wraps it in ADR-0008's envelope while the preview listener
writes it plain — a preview is a page an operator opens on a phone, and an envelope there would be an API item 3 says
this listener does not have. It names the peer's **address** and, where there is one, the node the tailnet named for it
(the caller's own identity), and it never names an `allowed_hubs` entry, the list's length or a path: acceptance bullet
5, asserted by a test that looks for every configured entry in the body. The daemon's English is a *log* line
(`detail=`), because it is a diagnosis and not an answer — `doctor`'s Tailscale finding is where it is read.

One `warn` per refusal names `peer=`, `reason=` and `node=`, and one `debug` per admitted `Node` names it back; both
listeners log the same two lines because they log the same decision.

## D5 — no identity header travels

Item 4 says "not by a header the caller supplies", and the answer is to add none: `forward::request_head` is unchanged,
its tests still pin it, and the peer verifies **the address it can see**. A header the hub added would be exactly the
kind of claim ADR-0016 distrusts, and a peer that believed it alone would be admitting anyone who could spell it.

## D6 — the boot says it at `debug!`, and the risk is a row

`tests/serve_cli.rs` pins the banner at exactly eight lines at the default filter, so a startup `warn!` about the
empty-list fallback would fail the gate — those stories's landmine. The boot states the trust config at `debug!`, and
the fallback is a row in the spec's **Known and accepted risks** table instead, where a reader looks for it.

## D7 — the gate stands a peer and a daemon in

`case_machine_trust` boots one machine twice (empty list, then `allowed_hubs = ["workshop"]`), puts a `tailscale` shim
first on `PATH` (the shape `deploy_machine_shims` already uses) that records every argv and answers `whois --json <ip>`
from a state file, and drives every request from a client **bound to `127.0.0.2`**.

| Arm | What it proves |
| --- | --- |
| empty list, stranger served | the ACLs-only fallback, and that it asks no whois at all |
| listed node served, `whois --json 127.0.0.2` in the log | the identity came from the **address**; no header reaches the child |
| another node | `403 peer_not_allowed`, one log line naming the peer and the reason, and no configured entry anywhere in the answer |
| a forged `Tailscale-User-Login` | unchanged: still `403`, because a claim is not a verification |
| the shim failing in `tailscaled`'s words | `403 peer_unverifiable`, and the body repeats none of that prose |
| `127.0.0.1` with a list configured | served, and the shim's log does not grow: the front door costs no child |
| the preview listener, both ways | plain-text `403` for the stranger; a listed peer reaches the listener's own `404` |

`127.0.0.2` is the load-bearing choice: without root it is the only address other than `127.0.0.1` a process can call
from, and it is on the *wrong* side of the front-door rule by design. `python3` binds the client, so the case prints one
`SKIP` line without it and `tests/trust.rs` covers the same arms in-process.

## D8 — the costs, said out loud

- **One child per request on the tailnet path.** A dev-server page loads thirty assets and a hub's preview hop pays one
  whois for each. There is **no cache**: an admission cached for a minute outlives the tailnet's own membership change,
  and a cache is a second trust decision with a lifetime. If it bites, a `Trust` cache with a TTL is the follow-up.
- **A `warn` per refused request.** A peer that retries fills the log; nothing throttles it.
- **`preflight::Tools` gained `tailscale`**, so the seam a test already uses for `git`/`df`/`du`/`openssl` names the
  whois too: seven test literals name it and five more take `..Tools::default()`. The alternative — a `Trust`-only
  program path — would have been a second seam for the same idea.
- **On a machine published through `tailscale serve`, this check sees the front door** (D10), so the acceptance bullet
  that names it is satisfied by the tailnet's ACLs and the `owner` check there, not by the whois. Recorded, not
  claimed.

## D9 — V11's row and the case that pins it, in one commit

`the gate` used to assert two sentences about V11 ("deferred to phase 3", and the story that would fill
it). This story **removes that rule** and gives V11 the same four rules every other row gets, in the same commit that
fills the cell — which is the shape that makes the row's automation a case that exists rather than a promise. The row
now says `**pass**`, names `case_machine_trust` and `tests/trust.rs`, and points at the risk row D6 records.

## D10 — what `tailscale serve` hides, and therefore what V11's verdict is

Deploy step 6 publishes the control origin with `tailscale serve --https=443 http://127.0.0.1:<control port>`
(`deploy::control_target`) and the preview origin the same way on 8444. `tailscaled` terminates the tailnet connection
and dials the local target, so **the listener sees `127.0.0.1` for every caller**: a browser, the CLI, a host probe —
and the hub's forwarded call, which is the one D3 exists to govern. A hub dials the peer's registry row, and ADR-0074
D4 requires that row to be an origin the peer's guard accepts — its `control_origin` or a loopback alias of it —
which on a deployed machine is the serve origin, so a forwarded call reaches the peer through serve and arrives as a
front-door caller.

Four consequences, and none of them is a bug in this story's code:

1. **The check bites where the address is real.** A machine whose `control_origin` is its listener's own
   tailnet-reachable address (bound beyond loopback, or reached by an address in `100.64/10`) — and any caller that
   dials the listener's socket directly — is verified exactly as D2/D3 describe. That is the arm `case_machine_trust`
   measures, and it is the arm `tests/trust.rs` pins.
2. **It does not bite behind serve**, and the address that would let it is not available: `tailscale serve` adds a
   header for the *user* (`Tailscale-User-Login`), never for the node, so the app cannot ask `whois` about a machine it
   cannot name. Trusting `X-Forwarded-For` or the login header to stand in for the node would be precisely the
   caller-supplied claim item 4 forbids.
3. **What controls a deployed machine instead** is the tailnet's ACLs (V11's own documented fallback) plus item 4's
   `owner` check, which `tailscale serve` makes meaningful because it is what adds the login header. That is a weaker
   claim than the story's, and it is the one the spec's "If it fails" column anticipated.
4. **The follow-up is named, not implied:** a machine-identity path that survives serve is a story of its own — either
   a listener a hub reaches directly with the origin the peer reports (which needs ADR-0074's row to carry a dial
   address distinct from the origin it claims) or a serve feature that reports the node. writes a deployed
   machine's config and is where the first would land.

So V11's Result cell says **`pass, with the fallback applied`**: the whois check accepts a listed hub and rejects
another tailnet node wherever it can see the address (measured, above), and a machine published through `tailscale
serve` falls back to ACLs plus the owner rule (recorded, in the spec's risk table). Saying `pass` outright would claim
the deployed path; saying `fail` would deny the measurement.

## What this machine could not measure

No `tailscaled`: `tailscale status` answers "failed to connect to local tailscaled", and no 100.64 address can be added
without root. So a real MagicDNS name, a real whois answer, a real tailnet refusal and TLS to a peer are **not** in this
record — every whois here is a shim's, the gate's two machines are loopback, and the one real arm is the daemon that is
not there. `doctor`'s Tailscale finding is what an operator reads when the real one is missing, and the spec's risk row
is what the fallback means.
