# A session is configured with Linear only where it may reach it

ADR-0099 put a third proxy on the machine and left the other half undone: nothing wrote the session's
`pi-home/mcp.json`, so a stored tool key was spent by a proxy no agent was configured to call. The gap was not wiring
— the proxy, the route and the key were all there — it was that two different questions had one answer between them.
**May this session reach a tool's host?** is the row's Internet-access checkbox, and it is topology: the join decides
it. **Is this session configured with Linear at all?** is a machine setting an operator chooses, because a Linear key
is a machine's and a toggle is a session's.

So `[linear] mcp` is the second question and the join is the first, and the decision recorded here is which of the two
the file may answer. It may answer neither on its own: a file the agent reads cannot make a proxy resolvable, and a
machine-wide key cannot be turned on per session.

## D1 — the gate is the network join, and the file is not the gate

A session whose row says no Internet access has no `mcp-proxy` alias on its network at all: `apply_proxy` leaves
`hs_mcp` off, Docker's embedded DNS answers nothing for the name, and the agent's client fails at resolution before it
sends a byte. That is ADR-0046's "the checkbox is topology" applied to a tool call, and it is what makes the file
safe to write for every session the key configures — a `url` this build composes correctly can still reach nothing,
which is exactly the state the toggle describes.

The consequence is the other direction, and it is why `off` writes **no file** rather than an entry marked
`enabled: false` or a server pointed at a dead name. A file that exists and cannot connect costs the model a server
the system prompt lists, on every prompt, for a tool that cannot answer; the honest spelling of "this machine has not
configured Linear" is the absence of the file. A session with Internet access off and a key that says `write` is the
one case that keeps an entry it cannot use, and that is deliberate: the operator asked for Linear and the session's
own row asked for no Internet, so one of the two facts is a per-session override this file must not second-guess.

## D2 — one word with three values, `off` by default

`[linear] mcp` is `off`, `readonly` or `write`. It is an enum serialized as a string rather than a boolean, because
there are two endpoints and choosing one is not a flag: `readonly` is `/mcp/readonly`, Linear's read tools with no
create or update, and the key's own permission in the workspace is the other half of what a session can do there.
`write` is `/mcp`.

`off` is the default because it is what every release before this one did — a machine that had stored a `linear` key
still configured nothing — so a promotion that changes nothing about this key changes nothing about a session. The
word is refused when it is not one of the three, in a sentence naming all three, and the mode's TOML spelling is one
table (`LinearMcp::WORDS`) so the word that parses and the word a refusal prints cannot drift.

## D3 — written on every start, and removed when the key says off

`DockerBackend::start_container` writes `pi-home/mcp.json` beside `pi-home/models.json`, before `Mounts::check`, for
the reason that file is written there: the agent's directory is a mount source, and `docker run -v` creates a missing
source as an empty root-owned directory. One call does both halves: `client_config` answers `Some` and the file is
written, answers `None` and the file is removed, ignoring `NotFound` — so the second start under `off` is uneventful
and the first clears what a previous start left.

**Rewrite every start, rather than seed only what is absent.** A resume finds the directory the previous container
left, with pi's own files in it, and `agent::ensure_home` deliberately seeds nothing over. This file is the exception
the same way `models.json` is: the key store is the machine's current state, and a file from an earlier start would
keep pointing at whatever the key said then — so `key set linear` on a running machine, or the key changing from
`write` to `off`, would be invisible to a session resumed after it. The config file is the single source of truth on a
resume because every start re-renders from it.

The write and the removal are one private function whose answer is data (`McpConfig::{Written, Removed, Nothing}`)
rather than a log line, so a unit test in `docker.rs` exercises the pair against a temporary directory with no daemon
attached. A failure is the container step's sentence naming the file and the error, the way the `models.json` arm
beside it fails.

## D4 — the url is the route, and the entry carries the dummy

The entry is one streamable-HTTP server named `linear`, with `url` composed from the same three values `Route::parse`
matches a request against: `MCP_PROXY_ALIAS`, `MCP_PROXY_PORT` and the tool row's endpoint. Nothing here spells a host,
a port or a path a second time. The read-only endpoint is the row's one whose last segment is the mode's own word and
the full one is the row's other, so a changed `key::TOOLS` row cannot leave a session's file pointing at a path the
proxy refuses.

`headers.Authorization` is `Bearer <dummy>`: the session's dummy key, the same value `models.json` carries, because
`pi-home` is inside a session mount and a real key there is a key the agent can read (ADR-0098). The dummy is enough
for the proxy — it validates the value, attributes the request to the session, and swaps in the stored key — and
`key::contains_real_key` covers this file as well as `models.json`.

Two fields are about the size of the server. `exposure: "deferred"` means the model reaches the tools with one
`tool_search` call rather than receiving every tool declaration in every prompt, which is what a server with dozens
of tools needs; `description` is the sentence that lists it until then. `timeout: 30` is under pi's sixty-second
default because a tool call that has not answered in half a minute is one the turn has moved past, and a failure the
model can read costs less than a prompt that hangs.

## D5 — `NO_PROXY` gains the MCP proxy's alias

`ENVIRONMENT` becomes `NO_PROXY=key-proxy,mcp-proxy`. Today nothing changes for a request, because the only proxy
variable a session's container carries is `HTTPS_PROXY` and this client's url is `http://` — but that is a fact about
which variable pi happens to honour, and a url's correctness should not depend on it. The two names a session may
resolve to a proxy it talks to in plaintext are precisely the two that must not be offered to the egress proxy, and
writing both is a claim the environment can be read for rather than one a future variable would silently break.

**This amends ADR-0099's D1, which said the environment was deliberately unchanged.** That was true of the change
that added the third container: no client was configured, so a name for it would have been an environment change
for a case nothing measured. It stops being true the moment this build writes the client that uses the name, and the
amendment there points here.

## Rollback

The key is additive on the running release and *not* readable by the one before it: that release's parser is a closed
list of keys, so a config file carrying `[linear]` is refused at load with `linear.mcp: unknown key`, and a machine
that goes back must remove the section by hand — the previous release's deploy cannot do it, because it refuses the
file at load before it writes anything. Nothing else about a rollback changes: the session's `mcp.json` is a file a
previous release never writes and never reads, so a tree switched back over a session simply stops configuring Linear.

`off` being the default is half of what keeps that cheap: a machine that never set the key renders no new section, so
its file stays byte for byte what the previous release writes and a rollback there is uneventful.

## What it costs, measured rather than assumed

- **One more small write per start, and one `unlink` when the key is off.** No daemon call, no extra mount, no
  process: the file is read by pi at startup the way `settings.json` is, and a session's `pi-home` grows by one file.
- **A configured server the session cannot reach is in every prompt and answers none of them.** The proxy is not on
  an Internet-off network, so the client's connection fails at DNS; pi reports the failed connection once after
  startup, and the entry stays in the system prompt's server list because `description` is what lists it. That is the
  cost this design pays to keep the key machine-wide while the reachability stays per session.
- **Nothing here counts Linear's tools.** The read-only endpoint's published set is its read tools, and nothing in
  this repository measures that number; it is the reason `exposure` is `deferred` rather than `direct`, and a set
  small enough to declare would make a different choice defensible.

## What pins it

`mcpproxy`'s unit tests pin the document: both modes name the tool table's own endpoints, the two urls differ only in
the endpoint they name, the file carries the dummy and no key `key::contains_real_key` knows, `exposure` is
`deferred`, `timeout` is 30, a description is present, the read-only description says it cannot write, and `off` is
`None`. `docker.rs`'s pin the pair: the helper writes exactly `client_config`'s text and `off` removes a file that was
there, then answers `Nothing` when it is asked again. `config`'s pin the word in both directions — all three values
parse, the rendered file reads back as the same value, an unknown word is refused naming the key and the three — and
the key table's count. `container`'s
pin the environment's `NO_PROXY` and `tests/agent.rs` the same value in a running container.
`scripts/verify.sh`'s `case_internet_toggle` seeds a real-looking provider key **and** tool key, sets
`[linear] mcp = "write"`, and asserts that both sessions' `pi-home/mcp.json` names
`http://mcp-proxy:8090/linear/mcp` with `"exposure": "deferred"` — and that neither real key appears anywhere under a
session's directory or in `docker inspect` of its container.

**Not measured here:** that `pi mcp list` answers `connected` for this entry, and that a `list_projects` call
answers this workspace's project. Both need a real Linear key and the internet, which a gate cannot have; they are
the demo that follows this change, the way ADR-0099's forwarded request waited for its own.
