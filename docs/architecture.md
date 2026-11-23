# Architecture

Read this before trusting the machine with a repository: the roles, the state machine, and where isolation actually
comes from. [Back to README](../README.md).

```text
                    phone / browser
                          │  HTTPS (Tailscale) or plain HTTP on the LAN
                          ▼
                 ┌──────────────────┐
                 │  control listener │  API + embedded PWA
                 └────────┬─────────┘
                          │
        ┌─────────────────┼──────────────────┐
        │                 │                  │
   hub role          server role        preview listener
   registry +        sessions,          /<host>/<id>/*  (own origin)
   forwarding        events, SQLite
                          │
                    provisioner ──► Docker container (pi) on an internal network
                          │              │
                    git worktree         ├─ key proxy  (provider key never enters)
                    agent/<id>           ├─ MCP proxy  (tool key never enters, only with Internet access)
                                         └─ egress proxy (allow-listed HTTPS)
```

**One binary, three roles.** Holosmith is one static binary, and the same binary is a server (runs sessions), a hub
(forwards to several servers, so one page drives several machines), or both. SQLite is bundled and the web app is
built in. `serve` reads `role` from the config: `hub`, `server`, or `both`. A hub has no Docker and no sessions; it
serves the app, the host registry, and forwards API and preview requests. A server runs sessions and owns the session
API. Every role answers the same paths.

**Sessions are a state machine.** A create answers `202` and the provisioner walks `starting → idle` (or `error`),
creating the network, worktree, container and agent. Turns move `idle → running → idle`; abort, steer, resume,
delete, idle-stop, timeout, token budget and disk caps all apply transitions through one table. Crash recovery
reconciles rows and containers on boot. The session screen is where you prompt it, steer or abort a turn, read the
changes view (the diff of `agent/<id>` against the base) and merge into one of `merge.targets` — `main` and `master`
by default.

**The transcript is an append-only event log.** One writer batches events into SQLite (`WAL`, `synchronous=FULL`) and
broadcasts after commit. Clients read replayable SSE streams and paginated history; tool output and images are files
under the session directory, not log payloads.

**Work happens on the host.** Each session has a git worktree and an `agent/<id>` branch. The server reads changes,
computes diffs, merges into a target branch, and optionally pushes; the container only edits files. The `.git` mount
is hardened read-only for hooks and config. Merges and pushes run on the host as you, with your own git credentials,
so a project must be one you can push from.

**Isolation is topology.** A session gets an `--internal` network. The only routes out are the key proxy (provider
calls, with a per-session dummy key — always joined, because a model call works either way) and, when Internet access
is enabled, the egress proxy (CONNECT/443, allow-list) and the MCP proxy (a tool's stored key, for the hosts
`key::TOOLS` names). Previews run on a separate listener and origin.

**Release and self-development.** The binary embeds the web app, migrations and the Docker build recipes.
`holosmith build` compiles in a pinned builder container; `smoke` proves a candidate against the real daemon with a
recorded turn; `upgrade` runs the review gate, smoke, image builds, drain, backup, release switch and health poll;
`rollback` walks the release tree one step back. Migrations are expand/contract, so switching back is safe. Nothing
on the wire promotes a release today, and the decision for when one does — a transient unit running the whole
container gate in place of the review flag, with the tailnet as the authority that presses — is
[ADR-0110](adr/0110-a-promotion-is-a-pass-of-the-gate-in-a-transient-unit.md).

**A project is a repository directly inside `projects_root`** with a real `.git` directory; submodules and
`extensions.worktreeConfig` are refused with `422 git_not_directory`, because those are the shapes that cannot be
mounted safely. Worktrees are made under `<projects_root>/.worktrees/<id>`. The new-session form asks for the
project's directory name, the model and thinking level, and whether the session gets Internet access; a dev server the
agent starts is linked on the session screen (the preview listener, port 8081).
