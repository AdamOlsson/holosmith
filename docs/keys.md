# Rotating a tool key

Read this when a session's Linear tools stop answering, or when you are rotating the key on purpose. The provider
keys a model call spends are [the README's quick start](../README.md); this page is about the **tool** table, which
holds a credential for a host the agent talks to directly.

[Back to README](../README.md).

## The four commands

```bash
printf '%s' '<new key>' | holosmith key set linear   # store the new one
holosmith key list                                   # what this machine holds
holosmith key revoke linear                          # forget it
holosmith doctor --full                              # does the stored one still answer?
```

`key set` reads the key from **stdin only**, which is why the first line is a pipe: a key on the command line is a
key in the process list, in your shell's history and in whatever a crash report captured. Piped, the command reads
to the end of the input with no prompt; run on a terminal it prints one sentence and turns the echo off, so the key
does not show as you type it. The value is never printed back. A `key list` line for a tool carries the word
`(tool)` — `linear (tool)` — because a provider id and a tool id are two different credentials and only one of them
has a proxy.

`key revoke linear` is the other half of a rotation and is idempotent: a tool with no stored key is the state you
asked for, so it prints one sentence and exits 0.

## A rotation needs no restart

Store the new key and the next tool call spends it. Nothing is restarted, and nothing has to be:

- the MCP proxy (`hs_mcp`) reads `keys.toml` **per request**, so the file is the live value rather than a startup
  copy;
- the container mounts the store's **directory**, not the file — a file mount pins the inode, so the temp-file +
  rename `key set` writes would be invisible to a process holding it;
- a session's own configuration never holds the real key. Its `pi-home/mcp.json` carries the session's **dummy**,
  and the real key is swapped in by the proxy on the way out.

So a rotation is a file write, and the first evidence that it worked is the next tool call. `holosmith doctor --full`
is what tells you *before* a session finds out.

## What `doctor --full` says about it

One `tool-key` line per tool the build knows:

| Line | What it means |
| --- | --- |
| `tool-key: pass: the stored linear key answered 200 for POST /mcp on mcp.linear.app, …` | the host accepted the stored key; the next tool call spends a good one |
| `tool-key: unknown: no linear key is stored on this machine; the operator runs …` | nothing is stored, so nothing was probed — the same sentence a session's tool call gets as a 503, naming `holosmith key set linear` as the fix |
| `tool-key: unknown: … this is not a verdict about the key` | the key was not measured: no network, no `openssl`, a host that closed or stopped mid-answer, or a status that says nothing about a key (a 404, a 5xx) |
| `tool-key: fail: the stored linear key is refused: POST /mcp on mcp.linear.app answered 401, …` | the key is wrong or has been revoked, and a new one is the fix |

Only the last one changes `doctor`'s exit code. Everything that could not measure the key is an `unknown` on
purpose: "could not measure" is not "measured a bad key", and a machine with no route to a tool's host is a machine
that runs sessions.

The probe reads its answer under its own 15-second deadline, not the 120 a session's forward waits for its first
byte: a forward may stream for the life of a session, and a `--full` run is something a deploy's health step waits
on. So a host that accepts the connection and then goes quiet is an `unknown` line, never a hang.

`--quick` does not ask the question at all. It is the unit's `ExecStartPre`, so a check there decides whether the
machine boots, and a stored tool key is not a requirement for serving.

The same rule reaches `server deploy` and `holosmith install`: their health step runs `doctor --full`, and a refused
key does not fail it. The step's line names the finding — and says it is the stored key rather than this machine's
fitness — so an install stays green on a machine that is serving. `holosmith key set <tool>` is what fixes it, and
re-deploying is not. `doctor --full` itself still exits 1, because the report is the measurement and the step is the
one deciding what a machine needs.

## What `key revoke linear` does to a running session

The next tool call gets a `503` whose body names the command that stores a key — `no linear key is stored on this
machine; the operator runs holosmith key set linear` — and no request reaches Linear. The session is not restarted,
stopped or marked `error`: the agent sees a failed tool call and goes on. `doctor --full` turns that same line into
`tool-key: unknown` with the same sentence, so the report and the session's refusal are one statement about one
machine.

## The proxy's log line

```bash
docker logs hs_mcp | tail
```

One line per request, at `info`, whose fields are the session it is attributed to, the tool, the host, the status and
the bytes each way:

```
INFO holosmith::mcpproxy: the MCP proxy answered a request session=1a2b3c4d5e6f7a8b tool=linear \
  host=mcp.linear.app status=forwarded bytes_up=46 bytes_down=2048
```

(`docker logs` prints it on one line, in front of a timestamp; the backslash above is this page's line break.)

A refused request is one line too, with `status=no-tool-key`, `status=bad-key`, `status=unknown-endpoint` or
`status=store` in place of `forwarded` and `session=-` when the request never reached a session's key. A `502` in a
session's tool call is `status=upstream` or `status=no-openssl` here, named by the same sentence the session got.

## Linear's own side

The key is created and revoked in the workspace's security settings — **Settings → API → Personal API keys** — not
by Holosmith. Revoke the old one there as the last step of a rotation, after `doctor --full` says `tool-key: pass`
for the new one. A key revoked there comes back `401` through the proxy, which is exactly what the finding reports,
and a key an admin has scoped to nothing comes back `403`.

Nothing in this build tests the workspace's permissions for you: the finding says whether the host accepts the
stored key, and the agent's own tool call is what says whether that key may read the project it is pointed at.

## When the report and the session disagree

`doctor --full` reads the store on the machine it is run on. A hub runs no sessions and stores no tool key, so its
line is `unknown` with the reason ("a hub runs no sessions, so nothing on it spends a tool key"), never a pass: run
the check on the **server** whose sessions call Linear. `holosmith status` names the proxies on this machine, and
`docker logs hs_mcp` on that server is where a forwarded call is attributed to a session.
