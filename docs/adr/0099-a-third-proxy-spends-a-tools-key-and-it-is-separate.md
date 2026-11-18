# The third proxy spends a tool's key, and it is not a route on the key proxy

`holosmith key set linear` stores a real Linear key in `<data>/secrets/keys.toml`'s `[tools]` table (ADR-0098), and
nothing spent it: the key proxy routes model providers only, so a tool key was a credential in a 0600 file that no
process read. A session's agent needs it to reach `mcp.linear.app` over MCP's streamable-HTTP transport, and the one
place this build may not put it is the session's own `pi-home` — that file is inside a session mount, so the agent can
read it.

So the machine gets a third plaintext-to-TLS proxy. `holosmith proxy mcp` is a **mode of this binary** like the other
two (ADR-0045's pattern: nothing new to build, nothing to keep in step), and `hs_mcp` is the container that runs it.

## D1 — a container of its own, because the key proxy is joined unconditionally

This is the whole decision, and it is not about the store. `hs_key` is joined to **every** session network
unconditionally (`DockerBackend::apply_key_proxy`), because a model call has to work whether or not the operator
ticked "Internet access", and it stands on the machine's `egress` network, which has real egress. A route on it for
`mcp.linear.app` would therefore make Linear reachable from a session whose row says `network = false` — which is
exactly what ADR-0046 refuses ("the checkbox is **topology**, not configuration") and what `docs/config.md` promises
("off means a session reaches no Internet").

A third container fixes that by taking the **egress proxy's** condition rather than the key proxy's:
`DockerBackend::apply_proxy` is one function over `(name, alias, what)`, and `hs_egress`/`hs_mcp` are its two callers
while `hs_key` keeps its own unconditional join. The checkbox is then the gate for a tool call as well as for a
download, and the boot measures it (D2).

The rest of the container is the key proxy's shape, because it reads the same file: the proxy image, the store's
**directory** mounted read-only at its own host path (a file mount pins the inode, so a rename is invisible — the V3
measurement ADR-0050 records), `--user` taken from the store's owner, `--cap-drop ALL`,
`--security-opt no-new-privileges`, `--restart unless-stopped`, on the machine's `egress` network under the alias
`mcp-proxy`. One function builds the arguments both keyed proxies share (`container::keyed_proxy_arguments`), so the
hardening and the mount cannot drift apart.

**The environment is unchanged, deliberately.** A session's container carries `NO_PROXY=key-proxy` and nothing for
`mcp-proxy`, because an MCP client's URL is plaintext on the session network: `HTTPS_PROXY` is the only proxy variable
the container is given, so an `http://mcp-proxy:8090/…` call never consults it. A client configured with `https://`
there would reach the egress proxy and be refused (port 8090 is not 443), which is a loud failure rather than a
leak — and adding a second name to `NO_PROXY` would change a session's environment for a case nothing has measured.
**Amended by ADR-0100:** that case is now measured by the file this build writes, so `NO_PROXY` carries
`key-proxy,mcp-proxy`; the url is still plaintext and `HTTPS_PROXY` is still the only proxy variable, but the two
names a session talks to a proxy in plaintext on are no longer a fact a url's correctness depends on.

## D2 — the boot measures the second half of the checkbox

`selftest` runs on a temporary internal network nothing is attached to: the internet, the host, the egress proxy's
alias and now the MCP proxy's must all be silent, and each silence means something because the same network reports
the egress proxy the moment it is joined. `mcp-proxy:8090` joins that list rather than getting a pair of its own: it
shares the egress proxy's condition, so a probe that reached it would be proving that a session with Internet access
off can reach a tool's host. `selftest::PROXY_TARGETS` is the two aliases, and a report that reached either is the
*wiring* finding (a join that should not have happened) rather than the isolation one.

## D3 — the route comes from the tool table, and nowhere else

`Route::parse` takes `/linear/mcp` and `/linear/mcp/readonly`: the first segment is a row of `key::TOOLS`, and the rest
of the path must be one of the endpoints that row lists. Both are constants, so a request cannot name a host, a port or
a path the table does not carry — ADR-0045's exact-list rule, and the reason the same table that `key set` validates
is the only source of a route. `/linear/sse` is 403 with a sentence naming the endpoints the row serves.

Selection never happens in this build: an MCP client is configured with a URL, and `pi/pi-home/mcp.json` is the
operator's file. A session reaches the proxy at
`http://mcp-proxy:8090/linear/mcp` with the **dummy** key from its `models.json` in `Authorization: Bearer` (or
`x-api-key`), because `pi-home` is a mount the agent can read and a dummy is what that mount may hold. The real key
comes out of the store per request, so `key set linear` on a running machine is a rotation with no restart
(ADR-0098's D3, now spent by something).

The streamable-HTTP shape is why the shared relay matters. `keyproxy`'s `upstream` already passed the method and every
header that is neither hop-by-hop nor a credential, and `forward` already streamed the answer byte for byte — MCP needs
exactly that (`Accept: application/json, text/event-stream`, `Mcp-Session-Id`, `MCP-Protocol-Version`, `DELETE`, an
optional `GET` stream). Rather than grow a second copy of the transport, the transport moved to `relay.rs`
(`Head`, `parse`, `read_head`, `read_body`, `respond`, `Upstream`, `forward`, `serve`), and the two keyed proxies are now two
*rules* over it: `keyproxy::Route` and `mcpproxy::Route`, each with its own refusal sentences. The credential headers
are dropped by that shared list (`relay::not_forwarded`, which carries `authorization` and `x-api-key`), so "the dummy
never reaches upstream" has one implementation rather than two.

## D4 — the fan-out says three, in one derivation of the names

The three names have one derivation each — `container::proxy_name`, `key::key_proxy_name`,
`mcpproxy::mcp_proxy_name`, over the config's container prefix — and `proxies::names` (`[egress, key, mcp]`) is that
derivation's spelling for the callers that hold a `Config`: `status` prints one `proxy <name>` line per container,
`doctor --full` one `proxy` finding **per container**, `uninstall` removes all three, and `deploy`'s step 4 reads all
three as its "already satisfied" state through `proxies::running` (`[bool; 3]`). The lists that hold no `Config`
compose the per-container functions directly and agree by construction: `selftest::Names` from the prefixes,
`DockerBackend::remove_network` from its own `egress_name`/`key_name`/`mcp_name`, and `smoke`'s teardown from the
three `*_name` functions with its own prefix. `reconcile` needed nothing but its test's name list: `hs_mcp` does not
carry a session id's shape, so the orphan sweep already leaves it alone.

## What it costs, measured rather than assumed

- **One `openssl` child per request, and a stream is a held one.** `forward` reads until the host closes, so a `GET`
  event stream keeps one connection and one child for as long as the server holds it — `relay::MAX_CONNECTIONS` slots
  in **this** process (64), not the key proxy's, because they are two containers. Nothing has measured whether Linear
  offers such a stream or how long it lasts; the cost of finding out is bounded by that number and by
  `FIRST_BYTE_TIMEOUT`, and a lost slot is a tool call that waits for another, never a model call.
- **Every MCP call is a fresh TLS handshake**, because `Connection: close` is what ends the TLS client. That is the
  key proxy's trade for a request/response API, and it is repeated here rather than special-cased.
- **`MAX_BODY` bounds requests, not responses.** An MCP answer carrying a long issue description is streamed to the
  client as it arrives and never held, which is the property `forward` was built for.

## The session's own configuration is not this change

Nothing in this build writes `pi-home/mcp.json`. The proxy, the container, the routes and the store's read are what
this change adds; the row that points a session's client at `http://mcp-proxy:8090/linear/mcp` with `models.json`'s
dummy is the operator's (or a later change's), the way ADR-0098 left the spending of a tool key to this one. That is
recorded here rather than hidden: a machine that runs this release and stores no tool key is unaffected, and one that
stores one has a proxy listening for a client it must configure.
**Amended by ADR-0100:** this build writes exactly that file — `pi-home/mcp.json`, one server on that url, from the
machine's `[linear] mcp` and on every start — so the row is no longer the operator's to write; the operator's half is
the key's own permission in the workspace, which is what bounds a session the file has pointed at the full endpoint.

## What pins it

`mcpproxy`'s unit tests pin the route (both endpoints, the tool table as the only source, `..` refused), the forwarded
request (method, `Accept`, `Mcp-Session-Id`, `MCP-Protocol-Version` and the body through; the dummy and any
`x-api-key` dropped; the real key in the row's header exactly once), and the container names. `relay`'s pin the shared
half: the head, the coalesced body, the header list, and the request builder. `container`'s pin the run vector (the
alias, the read-only directory mount, `--cap-drop ALL`, the `proxy mcp --listen 0.0.0.0:8090 --keys …` tail) and the
smoke prefix. `proxies`' pin that each starter runs one container and refuses without the image. `tests/key_proxy.rs`
drives the real listener for the refusals (401 with no dummy, 403 for a tool or endpoint the table does not name, 503
naming `key set linear`). `scripts/verify.sh`'s `case_mcp_proxy` covers the same refusals through the artifact and
`case_mcp_proxy_request` the bytes a **stand-in** `openssl` receives, `case_internet_toggle` the container's alias,
mount and capabilities plus the checkbox on a real session network, and `case_status`/`case_doctor_full`/
`case_uninstall`/`case_smoke` the three names in the reports that list them.

**Not measured here:** a real MCP call to Linear through the proxy. That needs a real key and the internet, which a
gate cannot have; it is the demo that follows this change, the way ADR-0050's provider path waited for its own.
