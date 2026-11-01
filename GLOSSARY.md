# Holosmith

A single-operator system that runs coding agents in containers on machines you own, and lets you drive them from a phone.

## Language

### Machines and roles

**Machine**:
One computer running the `holosmith` binary.
_Avoid_: Node, box, instance

**Role**:
Which parts of the system a machine runs: `hub`, `server`, or `both`.
_Avoid_: Mode, type

**Server**:
A machine that runs sessions. Has Docker, git and a project root.
_Avoid_: Worker, agent host, runner

**Hub**:
The single entry point you open in a browser. Forwards requests and previews to servers. Runs no sessions.
_Avoid_: Controller, orchestrator, gateway

**Host registry**:
The hub's list of servers it may forward to.
_Avoid_: Host list, node table

**Hosts seed**:
An optional file, copied to every machine, that seeds the host registry so the lists stay aligned.
_Avoid_: Config sync, inventory

**Local**:
The host name a server uses for its own sessions.
_Avoid_: Self, here

### Sessions and turns

**Project**:
A git repository on a server that sessions are created against.
_Avoid_: Repo, workspace

**Session**:
One unit of agent work: one worktree, one container, one agent process, one id.
_Avoid_: Task, job, run, conversation

**Turn**:
One prompt-to-settled cycle inside a session.
_Avoid_: Request, message, exchange

**Prompt**:
Starting a turn.
_Avoid_: Send, message, ask

**Steer**:
Adding guidance to a turn that is already running.
_Avoid_: Interrupt, inject, redirect

**Abort**:
Ending a turn early, on request or by timeout.
_Avoid_: Cancel, kill, stop

**Agent settled**:
The moment the agent has finished everything for the current turn and is waiting for input.
_Avoid_: Idle, done, finished

**State**:
One of `starting`, `idle`, `running`, `stopped`, `error`, `deleting`.
_Avoid_: Status code, phase

**Stopped**:
A session whose container is gone but whose history, worktree and events are kept, and which can be resumed.
_Avoid_: Ended, closed, paused, evicted

**Resume**:
Bringing a stopped session back to `idle` with the same id.
_Avoid_: Restart, reopen, wake

**Evict**:
Stopping the least recently active idle session to free a slot. Loses nothing.
_Avoid_: Kill, delete, prune

**Idle stop**:
Stopping a session that has been idle too long. Loses nothing.
_Avoid_: Reap, timeout, expiry

**Drain**:
Refusing new work and waiting for running turns to finish, so the machine can restart.
_Avoid_: Shutdown, quiesce

**Provisioner**:
The background work that turns an accepted session into a running one, reporting progress by step.
_Avoid_: Launcher, bootstrapper

**Stub backend**:
A session backend that produces sessions, turns and events without Docker, git or a real agent.
_Avoid_: Mock, test mode, dry run

**Fake pi**:
A stand-in agent process inside a container that replays a recorded session.
_Avoid_: Mock pi, test agent

### Events and history

**Event**:
One durable record in a session's stream, identified by a strictly increasing seq.
_Avoid_: Log line, message, item

**Seq**:
The per-session sequence number of an event. Gaps are legal only where compaction removed rows.
_Avoid_: Offset, id, index

**Delta**:
An incremental piece of model output that is appended to a message.
_Avoid_: Chunk, token, fragment

**Message snapshot**:
A single event that replaces the deltas of an old message with its full text.
_Avoid_: Compaction record, rollup

**Message**:
A completed unit of conversation output (text plus thinking) that deltas accumulate into.
_Avoid_: Bubble, response

**History**:
The events of a session that a client can read back, in pages.
_Avoid_: Log, archive, transcript

### Work and results

**Worktree**:
The git worktree that holds a session's files, on its own branch.
_Avoid_: Checkout, copy, clone

**Session branch**:
The branch `agent/<id>` that a session commits to.
_Avoid_: Agent branch, feature branch

**Base commit**:
The commit a session branch started from. "Ahead" is measured from here.
_Avoid_: Parent, origin, fork point

**Target branch**:
The branch a session's work is merged into.
_Avoid_: Main, trunk, destination

**WIP snapshot**:
A commit of a session's uncommitted work, stored under a hidden ref, that never touches the branch or working tree.
_Avoid_: Stash, backup, autosave, checkpoint

**DB backup**:
A copy of the database file taken before a promotion.
_Avoid_: Snapshot

**Changes view**:
A read-only diff of a session's branch against its target. Not a file browser.
_Avoid_: Diff viewer, explorer, file tree

**Merge**:
Landing a session branch on a target branch, on the server, never in a container.
_Avoid_: Apply, integrate, land

**Publish**:
Pushing a session branch to a configured remote.
_Avoid_: Deploy, release, ship

**`touches_ci`**:
Whether a session's diff changes CI configuration or hook directories, which warrants a warning before publish.
_Avoid_: CI flag, risk flag

### Network and credentials

**Key proxy**:
The container that holds provider API keys and swaps them for each session's dummy key.
_Avoid_: Auth proxy, gateway, sidecar

**Dummy key**:
The per-session throwaway credential the agent holds instead of a real key.
_Avoid_: Session key, placeholder, fake token

**Egress proxy**:
The only route from a session network to the internet. CONNECT to allow-listed hosts on 443 only.
_Avoid_: Firewall, forward proxy, gateway

**Allow-list**:
The set of hostnames the egress proxy will connect to.
_Avoid_: Whitelist, permitted hosts

**Session network**:
A private internal Docker network that only one session's container and the two proxies join.
_Avoid_: Sandbox network, bridge

**Internet access**:
The per-session choice of whether the egress proxy is connected. Off by default.
_Avoid_: Network flag, online mode

**Control listener**:
The HTTP listener that serves the app and the API.
_Avoid_: API port, main port

**Preview listener**:
A second HTTP listener that serves only session previews, on its own origin.
_Avoid_: Preview port, proxy port

**Control origin**:
The origin of the control listener.
_Avoid_: App URL

**Preview origin**:
The origin of the preview listener. Never the same as the control origin.
_Avoid_: Preview URL

### Operations

**Release**:
One built version of the binary, in its own directory, with the `current` symlink pointing at the live one.
_Avoid_: Version, build, deploy

**Promotion**:
Moving a candidate release to live: smoke test, drain, backup, switch, verify, or roll back.
_Avoid_: Deploy, upgrade, rollout

**Rollback**:
Pointing `current` back at the previous release.
_Avoid_: Revert, downgrade

**Doctor**:
The preflight and self-test command that decides whether a machine may run sessions.
_Avoid_: Healthcheck, verify

**Admin socket**:
The local unix socket used for commands that must never be reachable over HTTP.
_Avoid_: Control socket, IPC
