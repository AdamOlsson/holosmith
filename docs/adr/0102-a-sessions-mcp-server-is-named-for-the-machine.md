# A session's MCP server is named for the machine, not for the tool

ADR-0100 D4 wrote one server into a session's `pi-home/mcp.json` and called it `linear`. A session's agent had **no
Linear tools at all**. Measured inside the container, `pi mcp list --json` answered one server with `state: failed`,
`error: fetch failed` and `tools: []`, and it was not the machine's: it came from the repository's own
`/work/.pi/mcp.json`, which names `linear` → `https://mcp.linear.app/mcp`. A session's network is `--internal` with an
empty `[egress] allow`, so it cannot resolve that host, cannot complete OAuth, and reaches a tool's host only through
`hs_mcp` at `http://mcp-proxy:8090/<tool>/<endpoint>`.

The file was not the fault; the **name** was. pi's `loadMcpConfig` merges the project scope over the global one and a
project entry replaces a global entry of the same name, a session's `pi-home/mcp.json` is the global scope
(`$PI_CODING_AGENT_DIR/mcp.json`), and pi is launched with `--approve`, so the repository's entry won. Measured with
the proxy document in a global `mcp.json` beside the trusted project file: `holosmith-linear` — the proxy's url —
connected with 36 tools while `linear` — the public url — failed. The machine's entry was correct and unused, which is
the one outcome a configuration file exists to prevent.

A second, smaller defect was measured the same way. `doctor --full` printed `tool-key: pass: … so a session's next
tool call spends one this host accepts` on a machine whose `[linear] mcp` was `off`, and so had written no
configuration at all. That is true of the key and false of every session on the machine.

## D1 — the name is the machine's, so it carries the product's name

`mcpproxy::SERVER_NAME` is `holosmith-linear`, and the document's one entry is keyed by it. A name is the one value a
repository can collide with, and pi's precedence rule gives the repository the win: naming the server for the tool
hands any checkout the power to replace the machine's entry with one a session cannot reach. The name is therefore
the machine's, and it says whose it is.

**Only pi's server name changes.** The proxy's route is still `/linear/…`, `key::TOOLS`'s id is still `linear`, both
urls still spell it, and `key set linear` still stores the key — the id is the machine's own vocabulary for a tool and
nothing outside this file merges on it. The consequence the model sees is a tool-name prefix: `mcp__holosmith_linear__*`
rather than `mcp__linear__*`.

`SERVER_NAME` and the tool row's id are two constants rather than one, and a unit test asserts they are **not equal**,
because equality is the defect and not a shorthand. A future tool row inherits the same rule: the server a session is
configured with is named for the machine, whatever the tool is called.

## D2 — a repository's own server is visible, and it is left alone

A repository may still add a `linear` server of its own; nothing here reads, rewrites or refuses `/work/.pi/mcp.json`.
A tracked file in the project's checkout is the project's content, and this build rewriting it would dirty the
worktree the agent is committing from — the one thing a mount of the project's own files must not do. The fix is that
the machine's entry can no longer be **replaced**: both servers now appear in the session's list, the machine's
connects, and the repository's fails with `fetch failed`. That failure is a fact about the repository, it is visible
in `pi mcp list`, and `docs/troubleshooting.md` tells an operator what it means.

This repository's own tracked `.pi/mcp.json` was deleted for that reason: its `linear` entry was the failing server in
every session of this project's checkout, so it was where the defect was measured rather than a project's incidental
content. A session started outside a container reads the same server from `~/.pi/agent/mcp.json`, which is where the
deleted file's `toolExposure` policy belongs.

## D3 — the pass sentence names the mode, so it is a claim about a session

`doctor`'s `tool-key` finding now reads `[linear] mcp` and its pass arm has two sentences: a configured mode says
`… and [linear] mcp is readonly, so the next tool call of a session that may reach the proxy spends one this host
accepts`, and `off` says `… but [linear] mcp is off, so this machine configures no session with it: readonly or write
is what configures a session`.

The `off` sentence is about **this machine** and not about pi's merged view, because D2 leaves a repository's own
`linear` server where it is: "no session's agent is configured with a Linear server" was the first wording, and it is
false for a session whose project ships one. The configured sentence says *may reach the proxy* because `hs_mcp` is
joined by the egress proxy's condition, so a session with Internet access off has no tool call that spends anything
(ADR-0100).

The mode is the **Linear row's** and not a tool key's: `doctor::linear_mode` answers `[linear] mcp` for the row whose
id is `linear` and `None` for every other row, and a row with no mode gets the pass ADR-0101 recorded, which claims no
mode at all. D1's future second row therefore prints its own key and no `[linear]`.

Both sentences are still a **pass**: the host accepted the key, the key is not what is wrong, and a mode is not a
verdict about it. The check stays `--full`-only, one `Check::ToolKey`, and only a refusal changes the exit code. The
fail and unknown arms are unchanged — they make no claim about a session to be wrong about.

## Rollback

The name is a `pi-home` file the backend rewrites on every start (ADR-0100 D3), so switching back to the previous
release is uneventful: its next start of each session writes `linear` again, and nothing else on the machine carries
the new name — no config key is added, no schema or migration changes, no store row is written. A session resumed
rather than started keeps the file the newer release wrote until its next start, exactly as it keeps any other
rendering from an earlier start. `doctor`'s sentence changes with the binary, so the two releases do not read each
other's line.

## What it costs, measured rather than assumed

- **A tool's name changes for the model.** A prompt or a stored transcript that named `mcp__linear__*` names a tool
  the session no longer has. The server's `description` is unchanged and `exposure` is still `deferred`, so the
  sentence a deferred server is listed with is the same one. The prefix itself is pi's rule — a tool is named
  `mcp__<server>__<tool>` with every character other than letters, digits and `_` replaced, so `holosmith-linear`
  yields `mcp__holosmith_linear__*` — and no gate here pins it: the by-hand session was measured with
  `pi mcp list --json`, which showed the server connected and its tools under their own names, not their prefixed
  ones. A release that changed the rule would show in a transcript and in no case here.
- **A repository that names its own `linear` gets a failing server beside the machine's.** Visible in `pi mcp list`,
  not silent, and not something this build removes.
- **Nothing else moves.** The same one small write per start, the same mount, the same route, the same key path and
  the same dummy.

## What pins it

`mcpproxy`'s unit tests pin the document: the `entry` helper asserts `mcpServers` has exactly one key and that it is
`SERVER_NAME`, and a test of its own asserts the constant is `holosmith-linear` and not the tool row's id, with pi's
precedence rule as the reason. The existing assertions — both endpoints from the tool table, the dummy, `exposure`,
`timeout`, the descriptions, `off` being the one `Ok(None)` — now read a document under the new key, so a name that
drifted would fail them too. `doctor`'s unit test pins every pass arm: `off`, `readonly`, `write`, each naming its
mode word and what that mode means for a session, that an `off` line does not claim a session spends the key, and a
row with no mode (a synthetic second row) claiming no mode at all.
`scripts/verify.sh`'s `case_doctor_tool_key` runs the shipped artifact over a config with no `[linear]` section and
asserts the pass line names `[linear] mcp is off`, so the mode is measured in the artifact and not only in a unit
test. `case_internet_toggle` reads each session's `pi-home/mcp.json` and asserts the entry's name
(`"holosmith-linear"`), the proxy's url and the deferred exposure, so the rename is measured at the shipped-artifact
layer as well as in `mcpproxy`'s unit tests.

## Not measured here

That a real repository's `.pi/mcp.json` naming `linear` leaves the machine's `holosmith-linear` connected. That is
the measurement this decision was made from, taken by hand inside a session's container with `pi mcp list --json`,
and it needs pi, a container and a project checkout. No gate has pi: the smoke turn replays a fixture through
`fake-pi`, and `case_internet_toggle` starts real containers but never asks pi what it merged. The by-hand demo is the
one `AGENTS.md` records: `pi mcp list` shows `holosmith-linear` and one `list_projects` call answers.
