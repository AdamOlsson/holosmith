# Holosmith

Holosmith is a single-operator system for running coding agents in containers on machines you own, controlled from a browser or phone. An agent works in its own git worktree, on its own Docker network, with its own transcript; you watch it, steer it, stop it, review the diff, and land the result.

It is one static binary. The same binary is a **server** (runs sessions), a **hub** (forwards to several servers), or both.

## Dependencies

| Dependency | Needed for |
|---|---|
| Rust 1.99+ (edition 2024) | Building the binary |
| musl targets for `x86_64`/`aarch64` | Release artifacts (`scripts/build.sh`) |
| Node.js + npm | Building `web/` |
| Docker Engine | Servers: sessions, images, per-session networks |
| git 2.38+ | Servers: worktrees, merge trees, branches |
| Tailscale | Remote access, machine-to-machine forwarding |
| Linux + systemd | `server deploy`, `hub deploy`, `upgrade`, `rollback` |

SQLite is bundled; the web app is built into the binary. `pins/pi.txt` is the one pinned pi version the agent image installs.

## Run it

Development, no Docker (stub backend):

```bash
cargo run -p holosmith -- serve --backend stub --port 8080 --data-dir ./data
```

Then open `http://127.0.0.1:8080`.

Build and verify the artifact:

```bash
scripts/build.sh        # web app, both musl targets, static-artifact checks
scripts/verify.sh       # fmt, clippy, tests, migrations, build, embed, booted artifact
```

Production install on a Linux machine (from a release binary):

```bash
sudo holosmith server deploy            # install and start a server
sudo holosmith key set anthropic        # provider key on stdin
holosmith doctor --full                 # what this machine can actually do
```

A hub, or a second machine:

```bash
sudo holosmith hub deploy
sudo holosmith hub host add workshop https://workshop.tailnet.ts.net
```

The deploy is idempotent and `--dry-run` prints the plan. `server deploy` and `hub deploy` each install one half of a machine; running both leaves it `both`. See `docs/build.md` for the full build/release path and `holosmith --help` for every verb.

## Architecture

```text
                    phone / browser
                          │  HTTPS (Tailscale)
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
                    agent/<id>           └─ egress proxy (allow-listed HTTPS)
```

**One binary, three roles.** `serve` reads `role` from the config: `hub`, `server`, or `both`. A hub has no Docker and no sessions; it serves the app, the host registry, and forwards API and preview requests. A server runs sessions and owns the session API. Every role answers the same paths.

**Sessions are a state machine.** A create returns `202` and the provisioner walks `starting → idle` (or `error`), creating the network, worktree, container and agent. Turns move `idle → running → idle`; abort, steer, resume, delete, idle-stop, timeout, token budget and disk caps all apply transitions through one table. Crash recovery reconciles rows and containers on boot.

**The transcript is an append-only event log.** One writer batches events into SQLite (`WAL`, `synchronous=FULL`) and broadcasts after commit. Clients read replayable SSE streams and paginated history; tool output and images are files under the session directory, not log payloads.

**Work happens on the host.** Each session has a git worktree and an `agent/<id>` branch. The server reads changes, computes diffs, merges into a target branch, and optionally pushes; the container only edits files. The `.git` mount is hardened read-only for hooks and config.

**Isolation is topology.** A session gets an `--internal` network. The only routes out are the key proxy (provider calls, with a per-session dummy key) and, when Internet access is enabled, the egress proxy (CONNECT/443, allow-list). Previews run on a separate listener and origin.

**Release and self-development.** The binary embeds the web app, migrations and the Docker build recipes. `holosmith build` compiles in a pinned builder container; `smoke` proves a candidate against the real daemon with a recorded turn; `upgrade` runs the review gate, smoke, image builds, drain, backup, release switch and health poll; `rollback` walks the release tree one step back. Migrations are expand/contract, so switching back is safe.

## Where to look next

- `docs/build.md` — build, test, verify, deploy, release, and the by-hand credential checks.
- `GLOSSARY.md` — the vocabulary: what a session, a turn, a stream, a release and a drain mean here.
- `docs/adr/` — the decisions and why they were taken.
- `AGENTS.md` — the repository rules for agents working on the code.
