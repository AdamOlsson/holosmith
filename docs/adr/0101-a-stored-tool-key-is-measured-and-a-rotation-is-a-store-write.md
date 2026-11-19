# A stored tool key is measured, not trusted, and a rotation is a store write

`printf '%s' '<new key>' | holosmith key set linear` wrote a value nothing checked. The MCP proxy loads the store per
request (ADR-0098 D3), so the first evidence a rotation worked was a session's own tool call — and a key a workspace
had already revoked, a key pasted with a character missing, or a key an admin scoped to nothing all look identical on
this machine: a `[tools]` row with a string in it. Nothing on the machine ever asked the tool's host whether it
accepts that string.

So `doctor --full` asks. One `tool-key` finding per `key::TOOLS` row probes the row's host with the stored key, and
the second half of the decision follows from the first: rotation is a **store write**, not a restart, and a check
that measures the stored value is what makes "no restart" safe to say.

## D1 — one uninitialised `tools/list` call, and only a refusal is a failure

The probe is `POST <the row's first endpoint>` with the body `{"jsonrpc":"2.0","id":1,"method":"tools/list"}` — the
call `case_mcp_proxy_request` already sends, a call a streamable-HTTP MCP server answers without a session and
without spending a tool. It is not `initialize`: a probe that had to complete a handshake would be measuring Linear's
protocol state as well as this machine's key, and a key that is fine would report as a failure whenever the handshake
changed. `mcp.linear.app` answers `tools/list` unauthenticated with a `401`, which is the fact the check needs.

The statuses are read as three different kinds of statement:

| answer | finding | why |
| --- | --- | --- |
| `2xx` | **pass** | the host accepted the key, so the next tool call spends one it accepts |
| `401`, `403` | **fail** | the key is wrong, revoked or unentitled, and a new one is the fix |
| any other status (`400`, `404`, `5xx`, …) | **unknown** | an endpoint that moved or a host having a bad minute says nothing about the key |
| no status at all (no `openssl`, a host that closed, a bound) | **unknown** | "could not measure" is not "measured a bad key" |

**Amended by ADR-0102:** the pass is two arms — a configured mode says the next tool call of a session that may reach
`hs_mcp` spends the key, `off` says this machine configures no session with it — and the operator page has five
findings rather than four.

Only the failure changes the exit code. That is ADR-0044's rule one check over: a `doctor` that fails because a
tool's host was down would refuse a machine that serves sessions perfectly, and the same rule is why a machine with
**no** stored key is an `unknown` rather than a failure — it is a supported machine, and the sentence it gets is the
MCP proxy's own 503 (`mcpproxy::no_key_sentence`), so a report and a session's refusal say one thing.

The version the probe announces is `mcpproxy::MCP_PROTOCOL_VERSION`, which is **this build's probe's** alone: a
session's client negotiates its own `MCP-Protocol-Version` and the proxy passes it through, so the constant is not a
fact about the proxy. The `unknown` row is what a host that has moved on costs — it answers `400`, the check stops
measuring a good key, and the line names the status that answered rather than claiming the key is bad. That is the
trade D1's "only a refusal is a failure" rule already makes; a probe that had to negotiate to stay current would be
measuring the host's protocol state instead.

A **hub** is asked nothing: it runs no sessions, so nothing on it spends a tool key, and the check returns before it
reads the store or touches the network. The sentence says exactly that, in this check's words rather than the
container checks' — theirs said a tool key needed a session to spend it, which a server's line measures without one.

## D2 — `--full`, because a stored key is nobody's boot

`doctor --quick` is the unit's `ExecStartPre` (spec item 10 §5), so a check there can refuse a boot. A stored tool key
is not a requirement for serving: a machine that has never stored one runs sessions that never call a tool, and a
workspace that revokes a key must not turn the operator's next restart into a refusal. So the check joins `--full`'s
list — the post-deploy moment, where "is everything an operator will expect actually there" belongs — next to
`openssl` (ADR-0071 D9) and for its reason. `--quick` gains nothing at all.

The rule reaches the other caller of `doctor --full` too: `server deploy`'s health step reads that report through one
filter that leaves the tool key out, so the finding is printed verbatim as a remark on a green line and refuses
nothing — `holosmith key set <tool>` is the fix, not a re-deploy. The report keeps its exit code; the step is the one
deciding what a machine needs.

## D3 — the probe is the proxies' TLS client, not a second one

All TLS in this build is `relay::forward`'s `openssl s_client`, because the artifact is static and has no TLS stack
(ADR-0048), and ADR-0099's rule is that the two keyed proxies cannot become two readings of HTTP/1.1 or two TLS
clients. A `doctor` probe with its own client would be the third reading.

So `relay` gained `request(host, bytes, bound)`: the same spawned client (`s_client_args` is one list, `spawn` one
function, the stderr drain one function), one request written, and the answer read to its end under a new
`MAX_ANSWER` bound and **one deadline over the whole read**, which is the caller's to choose. The bound exists
because this caller wants a **verdict**, not a stream: `forward` copies bytes on as they arrive and holds nothing,
and a probe that did the same would be a second buffering policy. A `Failure` from it is the same enum a proxy
answers a session with, so the sentence an operator reads in `doctor` is the one a failing tool call carries.

The deadline is not a detail of the read but the reason the function is safe to call from a command somebody is
waiting on: `Connection: close` is a request header a host may ignore, so an SSE-framed `200` held open or a
connection that stalls after one byte would wedge a synchronous `doctor --full` — and a deploy's health step runs it.
So the probe passes `mcpproxy::PROBE_TIMEOUT` (15 s), a forward keeps what it always had (a bounded first byte, an
unbounded stream, because a session's answer may be an event stream held for that session's life), and the elapsed
case is `Failure::Bound`, whose sentence already exists.

The request bytes are `mcpproxy`'s, composed by `relay::Upstream::request` — the same builder a session's call goes
through — over a `Head` the probe fills in. The endpoint is the row's first and the host is the row's, so no argument
can name a host or a path `key::TOOLS` does not carry (ADR-0045, ADR-0099's D3). A probe is therefore evidence about
the path a tool call takes, not about a path invented for it.

`doctor` is synchronous; the probe is `async` because the relay is. The check builds a
`tokio::runtime::Builder::new_current_thread()` runtime and blocks on it inside the closure that is only spent when a
key is stored, which is `forward`'s own test shape. `doctor` is never called from inside a runtime (its two callers
are the command and `deploy`'s steps, both synchronous), and a runtime that cannot be built is an `unknown` sentence
rather than a panic.

## What was refused

- **A `Machine` trait method for the probe.** `tests/doctor.rs` drives `Depth::Full` over a `FakeMachine` and a store
  with no key, so that path is a `unknown` with no network; the five findings are unit-tested over the arm function
  (`doctor::tool_key_from`), and `scripts/verify.sh`'s `case_doctor_tool_key` measures the real subprocess path with a
  stand-in `openssl`. A fifth trait method would have been a seam for one sentence.
- **Checking the key against the project it is pointed at.** Whether the key may read a given team is Linear's to
  answer and a per-project fact, not a machine fact; the tool call is where that refusal arrives.
- **Probing on `--quick`, on a timer, or at boot.** A boot that waits on a tool's host is a boot a network outage can
  refuse, and a scheduled probe is a request nobody asked for.
- **A second status arm for a workspace's own error envelope.** A `200` carrying a JSON-RPC error is still a `pass`:
  the check is "does the host accept this credential", and the agent's tool call is where the tool's own complaints
  belong.

## What pins it

`doctor`'s unit tests pin the five findings — a pass for a configured mode, a pass for `off`, an `unknown` with no
key, an `unknown` that could not measure, a `fail` on a refusal — and the hub arm, including the assertion that a hub
does not read the store it was given an unreadable copy of. A row that is not Linear's gets the pass with no claim
about a mode: that is the row ADR-0102 D1 anticipates rather than a sixth finding.
`tests/doctor.rs` pins the report's shape — nothing at all on `--quick`, exactly one `unknown` per `key::TOOLS` row
with no key stored, one `fail` per row over an unreadable store — and needs no socket for any of it. `relay`'s pin
the one argument list and the answer's bounds, over bytes and over time. `mcpproxy`'s pin the probe's request bytes
(the row's endpoint, `POST`, `Accept`, `Content-Type`, `MCP-Protocol-Version`, the stored key once, the body) and the
status-line reader. `deploy`'s pin the rule that a refused stored key is named on the health step's line rather than
failing it. `scripts/verify.sh`'s `case_doctor_tool_key` runs the artifact three times with a stand-in TLS client — no
key stored, then `401`, then `200` — and asserts each line, plus that `--quick` says nothing about it. `docs/keys.md`
is the operator's page: the four commands, why no restart is needed, and the five findings.

## Not measured here

That Linear accepts a real key. It needs a key issued in the workspace's security settings and the internet, which no
gate may have. The by-hand half is recorded in `AGENTS.md` ("The stored tool key"): one `list_projects` call inside a
live session, `docker logs hs_mcp | tail` showing `session=… tool=linear … status=forwarded`, and `doctor --full`
showing `tool-key: pass`.
