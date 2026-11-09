# Holosmith: Spec Amendments

Nov 1, 2026 · Response to the Oct 31 spec review · Applies to Holosmith: Spec

Each item gives the standard fix and text that can be pasted into the spec. Numbers match the review. Decisions D1 to D3 are settled and folded into the items below (listed at the end).

## Summary

| # | Gap | Resolution |
| --- | --- | --- |
| 1 | Writable .git mount | Keep worktrees; hooks and config mounted read-only; residual risk accepted (see Known and accepted risks) |
| 2 | Egress and tokens | Internal per-session networks with the proxy as the only route; a key proxy keeps API keys out of containers |
| 3 | Same-origin preview | Separate preview origin on its own listener |
| 4 | Browser and hub exposure | Host, Origin and header checks; tailnet-only host registration |
| 5 | Durability vs batching | Group commit, broadcast after commit, synchronous=FULL |
| 6 | Restart name conflict | --rm, forced rm before start, tracked stops |
| 7 | Create timeout, duplicates | Async create (202) with idempotency key |
| 8 | State machine | Explicit transitions, stopped state, resume |
| 9 | Eviction and delete data loss | Evict means stop; snapshot and fetch before any delete |
| 10 | Self-dev rollback | Release symlinks, host smoke test, auto-rollback, expand/contract migrations, drain |
| 11 | Notifications | Web Push with VAPID |
| 12 | Seeing and shipping results | Read-only changes view; server-side merge into a target branch; optional publish |
| 13 | Runaway cost | Turn timeout, idle stop, token budget |
| 14 | Disk | Free-space floor and per-session soft cap; cache pending D2 |
| 15 | Event growth, tool output | Coalescing, snapshots, paginated history, tool output files |
| 16 | Abort hang, hub gaps | Abort ladder, preview routing through the hub, hosts seed file |

## A. Isolation and security

### 1. Worktrees with a hardened .git mount

Decision: keep one git worktree per session, as in the original spec. A worktree needs the project's `.git` directory writable, which is the exposure the review flagged, so the mount is hardened instead of removed.

Host commands (I7), as in the original spec:

```
git worktree add -b agent/<id> <projects>/.worktrees/<id> HEAD
git rev-list --count <base_commit>..agent/<id>
git worktree remove --force <projects>/.worktrees/<id>
git branch -D agent/<id>
git worktree prune
```

Container mounts. Host paths are kept identical inside the container, because the worktree's `.git` file points at `<project>/.git/worktrees/<id>`:

```
-v <worktree>:/work
-v <project>/.git:<project>/.git
-v <project>/.git/hooks:<project>/.git/hooks:ro
-v <project>/.git/config:<project>/.git/config:ro
-e HS_SESSION_ID=<id>
```

- Hooks and the repo config are what let a repository run programs on your host (hooks, `core.fsmonitor`, `core.sshCommand`, aliases, credential helpers). With both read-only, the agent cannot plant any of them. With `--cap-drop ALL` it cannot unmount or remount, and renaming a mount point fails.
- Refused with 422 at session creation: a `.git` that is not a directory, a repository with submodules (their hooks live under `.git/modules`), and `extensions.worktreeConfig` enabled (it would allow a per-worktree config the agent can write).
- Not prevented: the agent can still rewrite or delete any branch in the shared repository, including main, and can damage objects or other sessions' worktree metadata. This is an accepted risk (see Known and accepted risks). The optional `project_backup` setting writes `git bundle create <data>/backups/<project>-<date>.bundle --all` before the first session of each day on a project and keeps the last seven.
- No fetch step is needed anywhere: the session branch and its snapshot refs already live in the project repository.

### 2. Egress enforcement and credentials

Environment proxy variables are advisory, so enforcement moves to the network.

**Per-session network access.** The New session screen has an Internet access checkbox, sent as `network` (boolean) in `POST /api/sessions` and stored on the session, so a resume keeps it. The default comes from the config key `network_default`, which ships as off.

- The model provider is reachable either way, through the key proxy, which is connected to every session network. The checkbox controls everything else.
- On: the egress proxy is connected to the session network and the allow-list below applies.
- Off: the egress proxy is not connected, so the container can reach only the key proxy. `npm install`, `cargo fetch` and git over HTTPS fail at once; dependencies must already be in the project or the image.
- Proxy environment variables are the same either way. The startup self-test also checks the off case: a probe on a session network without the egress proxy must reach nothing except the key proxy.

* One internal network per session: `docker network create --internal agents_<id>`. The container joins only that network. The proxy container is also attached to a normal `egress` network and connected to each session network with `docker network connect`. Containers cannot reach each other or the internet directly, and there is no gateway to the host.
* Containers get `HTTPS_PROXY=http://egress-proxy:3128` and resolve nothing externally; the proxy resolves names for them.
* Proxy rules: CONNECT to port 443 only, only to allow-listed domains, and refuse any destination that resolves to loopback, link-local, RFC 1918 or 100.64.0.0/10 (the Tailnet). Log host and bytes per request, using the session id as proxy username for attribution.
* Startup self-test: a probe container on a temporary internal network tries a direct connection to the internet and to the host. If either succeeds, the server refuses to start sessions.
* Phase 2 exit check: `curl --noproxy '*'` from a container fails, a request to a non-listed host through the proxy returns 403.
* To verify in the vertical slice: that the host can reach a container's IP on an internal network (needed for the preview). Fallback: run the preview proxy as a container on the session network.
* Residual risk to state in the spec: every allow-listed host is a possible exfiltration channel (HTTPS is opaque to the proxy). Keep the list minimal and prefer registry mirrors.

**Credentials (decided in D1: API keys).** Tokens never enter containers.

- A key proxy container holds the provider API keys in a file readable only by the proxy. It joins each session network and the `egress` network. Keys are never written to the pi agent directory, an environment variable or a mount in any session container.
- Each session gets a random dummy key. Pi is configured with the provider base URL `http://key-proxy:8080/<provider>` and that dummy key. The proxy checks the dummy key, swaps in the real key, forwards over HTTPS, and logs requests and bytes per session. Deleting a session revokes its dummy key.
- Provider hosts are removed from the container allow-list, so the egress proxy permits only registries and similar hosts. This shrinks the exfiltration surface. The key proxy accepts connections only from session networks and forwards only to the provider hosts.
- Pi's agent directory is per session (`<data>/sessions/<id>/pi-home`), seeded from a template with model preferences and no secrets. The shared login directory and the concurrent token-refresh problem are gone.
- Checked against pi's docs on Nov 1, 2026: models.json can set baseUrl and apiKey on a built-in provider ID and keeps that provider's built-in models. The apiKey accepts a literal, $ENV\_VAR or !command. So each session's pi-home/models.json holds the key proxy URL and the dummy key. Launch pi with PI\_CODING\_AGENT\_DIR pointing at that directory, NO\_PROXY=key-proxy (pi honours HTTPS\_PROXY, and provider calls must skip the egress proxy), and PI\_OFFLINE=1, PI\_SKIP\_VERSION\_CHECK=1 and PI\_TELEMETRY=0 so pi makes no other outbound calls. Launch it attached (`docker run -i`, not `--detach`) so the server holds pi's stdio: that is its RPC channel, it is where "ready" comes from (the response to a `get_state` command), and it is the only source of the exit code and the stderr that item 6 classifies. The session's model and thinking level are passed as `--model`/`--thinking` as well as written into the seeded `settings.json`, because pi resolves neither from the file alone when `PI\_OFFLINE=1`. Project trust is approved with `--approve` (open question 9, closed). The directory is mounted at **its own host path** rather than at a path under the image's home, because item 1 keeps host and container paths identical and Docker creates a missing mount *destination* as a root-owned directory an image's own user cannot write. Still to confirm in the vertical slice: streaming and tool calls work through the key proxy, and get\_available\_models lists the built-in models with only a dummy key and PI\_OFFLINE set. The spend-limited key fallback stays as a contingency.

### 3. Separate origin for previews

- The server runs a second listener, the preview listener, that serves only `/<host>/<id>/*` (HTTP and WebSocket) and has no API routes. Publish it as its own origin, for example `tailscale serve --https=8444`. The control UI is on 443, so an agent-written dev build can never share an origin with it.
- WebSocket upgrades on the preview listener check that the Origin is the preview origin.
- Routing for other machines: the hub's preview listener forwards `/<host>/<id>/*` to that machine's preview listener over the Tailnet, including WebSocket upgrades. Local sessions use the host name `local`.
- Residual risk: previews of different sessions share the preview origin with each other. Acceptable because they are all yours.

### 4. Browser and hub exposure

On the control listener:

- Check the `Host` header against an allow-list (the Tailnet name, localhost). This stops DNS rebinding.
- Check `Origin` on every non-GET request against the control origin.
- Every non-GET request must be `Content-Type: application/json` and carry `X-Holosmith-Client: 1`. A cross-origin page cannot send that without a CORS preflight, and the server sends no CORS headers.
- GET endpoints are read-only and never change state.
- When behind `tailscale serve`, require the `Tailscale-User-Login` header to equal the configured owner.
- Machine to machine: a machine accepts forwarded calls only from peers listed in `allowed_hubs`, verified with the Tailscale LocalAPI whois of the remote address. Browser-facing checks apply at the hub.

Host registration (`POST /api/hosts`): the URL must be http(s), the host a MagicDNS name or an address in 100.64.0.0/10, the port in an allowed range; redirects are not followed; and the `/api/health` reply must carry the Holosmith name and version fields.

## B. Correctness and lifecycle

### 5. Durable events and batching

One writer task takes events from all sessions, commits them in a single transaction every 25 ms, and only then broadcasts them in order. SQLite runs with `journal_mode=WAL` and `synchronous=FULL`, so a client never sees an event that a crash or power loss can undo (otherwise a restarted seq counter would make clients drop new events). The cost is up to 25 ms of extra latency. Replace the earlier 100 ms figure.

### 6. Container naming and exit handling

```
docker run -i --rm --init --name s_<id> --network agents_<id> ...
```

- `--rm` means a stopped container never leaves its name behind. Before every start, run `docker rm -f s_<id>` and ignore not-found.
- Session data lives in host directories, so removing the container loses nothing.
- Startup reconcile uses `docker ps -a --filter name=s_`, removes any leftover container, then restarts sessions that were live.
- The server records an expected-stop flag before every `docker stop -t 10`. Exit with the flag set means `stopped`. Exit code 0 without the flag means `stopped` with reason `pi_exit`. Any other exit means `error` with the last stderr lines. This replaces the earlier clean-exit-means-ended rule.

### 7. Creating a session

`POST /api/sessions` becomes asynchronous and idempotent:

1. Validate synchronously (422 for bad input, 409 if the cap is reached and nothing can be evicted). The body is `{project, model, thinking, network}`.
2. Insert the row in state `starting`, with the client's `Idempotency-Key` stored in the unique column `client_key`. A repeat of the same key returns the existing session.
3. Return 202 with the session. The client opens the event stream immediately.
4. A background provisioner runs worktree, network, container, pi ready, and emits `session.status` with `step` set to `worktree`, `network`, `container` or `pi` for progress. On failure it undoes completed steps and sets `error` with a reason.
5. At startup, any row still in `starting` is either resumed or set to `error`, and orphaned networks, worktrees and containers are removed.

This removes the conflict between the 5 second hub timeout and provisioning time, and the earlier contradiction between waiting for ready and returning `starting`.

### 8. State machine

States: `starting`, `idle`, `running`, `stopped`, `error`, `deleting`. The state `ended` is removed. `interrupted` is a reason on a `session.status` event, not a state.

| From | Trigger | To |
| --- | --- | --- |
| none | create accepted | starting |
| starting | pi reports ready | idle |
| starting | provisioning failure | error |
| idle | prompt | running |
| running | agent\_settled | idle |
| running | abort completes | idle |
| running | abort ladder or turn timeout kills container | stopped, then immediate resume |
| idle | evict, idle stop, drain | stopped |
| stopped | resume | starting |
| starting, idle, running | container exits unexpectedly | error |
| error | resume (clone present) | starting |
| any | delete | deleting, then removed |
| server crash | recovery on startup | starting, then idle; a running turn is recorded as `interrupted` |

- New endpoint: `POST /api/sessions/:id/resume` (202). Prompt, steer and abort on a stopped session return 409 `session_stopped`; the UI resumes and retries.
- The cap counts live sessions: `starting`, `idle`, `running`. A resume on a full machine evicts the least recently active idle session first, or returns 409.

## C. Data safety

### 9. Eviction and delete

- Eviction and idle stop stop the container (state `stopped`). History, worktree, events and images are kept. Nothing is deleted by the cap.
- A snapshot script in the image, run with `docker exec s_<id> hs-snapshot`, records uncommitted work without touching the branch or working tree:

```
#!/bin/sh
set -e
cd /work
export GIT_INDEX_FILE=/tmp/hs-idx
ref=refs/holosmith/wip/$HS_SESSION_ID
rm -f $GIT_INDEX_FILE
git read-tree HEAD
git add -A
tree=$(git write-tree)
if [ $tree != $(git rev-parse HEAD^{tree}) ]; then
  c=$(git -c user.name=holosmith -c user.email=holosmith@localhost commit-tree $tree -p HEAD -m 'holosmith snapshot')
  git update-ref $ref $c
else
  git update-ref -d $ref || true
fi
rm -f $GIT_INDEX_FILE
```

- The snapshot runs at every `agent_settled`, before any stop, and before delete.
- Delete flow (returns 202, state `deleting`): abort if running; snapshot; `docker stop`; `git worktree remove --force`; remove the session directory and network; delete the row. If the snapshot fails, delete returns 409 `unsaved_work` unless `?force=true`.
- Kept in the repository: branch `agent/<id>` if it has commits beyond the base and is not merged into a target branch, and `refs/holosmith/wip/<id>` if it differs from the branch. Recover with `git branch rescue refs/holosmith/wip/<id>`.

## D. Self-development

### 10. Promotion, rollback, migrations, drain

Releases live in `releases/<tag>/` with a `current` symlink that systemd runs. Promotion script:

1. Build in a pinned builder container, not on the host, so `build.rs` and proc macros never run host-side. Review the diff first (item 12); that review is the real gate.
2. Smoke test on the host: run the candidate with a smoke config (alternate port, temporary data directory, `container_prefix=smoke_`, `network_prefix=smoke_`) against the real Docker daemon, using a fake pi in the image that replays the session recorded in phase 1. Create a session, run a turn, delete it.
3. Drain: `holosmith admin drain --wait 10m` over a local unix socket. It refuses new creates and prompts (503 `draining`), waits for running turns, then stops idle sessions with reason `server_restart`.
4. Back up the database (`VACUUM INTO backups/app-<ts>.db`, keep five), switch the symlink, `systemctl restart`.
5. Poll `/api/health` for 20 seconds. If the new version does not answer, switch the symlink back and restart.
6. Sessions that were live before the drain return to idle on startup.

In v1 these steps are implemented by `holosmith upgrade <binary>` and `holosmith rollback` (see Single binary and CLI), not by a separate script.

Migrations are forward-only numbered SQL files embedded in the binary, applied at startup in a transaction. They follow expand/contract: a release may add but never drop or rename anything the previous release uses; cleanup ships one release later. That makes the symlink rollback safe without restoring the database.

Dev instance: it runs in the dev container with `--backend stub` (no Docker; the fake pi replays the recorded session) on its own port and data directory. It tests UI, API and event handling. Docker, git and proxy paths are covered by the host smoke test in step 2. State this limit in the spec.

A crash restart (without drain) keeps the behaviour in the lifecycle section: running turns are recorded as interrupted.

## E. Phone experience

### 11. Notifications

- Web Push with VAPID keys generated at first start. Requires HTTPS (`tailscale serve`) and, on iOS, the app installed to the home screen.
- Endpoints: `POST /api/push/subscribe` and `DELETE /api/push/subscribe`, table `push_subscriptions`.
- Triggers: a turn finished after running longer than 20 seconds; `session.status` error; a budget or timeout stop. Not sent while an SSE stream for that session is open.
- Payload carries only `{host, session_id, kind}`, never content. Tapping opens that session.
- Each machine has its own keys, so the web app subscribes on every machine it uses (through the hub). The server, not the containers, needs outbound access to the push services.

### 12. Seeing, merging and shipping results

A read-only changes view (a diff, not a file browser, so the non-goal stands), a server-side merge into a target branch on that machine, and an optional publish of the session branch to a remote. The intended flow is: start a session, let the agent work, review the changes, merge. All git work for review and merge runs on the host in the project repository; containers never merge.

- `GET /api/sessions/:id/changes` returns `{branch, base_commit, head, target, ahead, behind, dirty, mergeable, files:[{path, status, additions, deletions}], touches_ci}`. `behind` counts commits on the target that the branch lacks. `mergeable` comes from `git merge-tree --write-tree` (git 2.38 or newer), which touches no working tree. `dirty` means uncommitted work in the session's worktree.
- `GET /api/sessions/:id/changes/diff?path=` returns the unified diff, capped at 256 KB, computed with `--no-ext-diff --no-textconv`.
- `POST /api/sessions/:id/merge` with `{target, strategy, message?, delete_session?}`. The strategy is `ff-only` (default), `merge` or `squash`. The `target` must be a local branch listed in the config key `merge.targets` (default: `main` and `master`, whichever exist); the UI preselects the first. Rules:
  1. Refused with 409 `session_running` while a turn runs, and with 409 `dirty_worktree` while the session has uncommitted work. The UI then offers to prompt the agent to commit with a clear message. Only commits are merged.
  2. One merge per project at a time. The server creates a temporary detached worktree at `<data>/merge/<id>` from the target, runs the merge there with `-c core.hooksPath=/dev/null` so that your hooks never run agent-written code, and removes the worktree afterwards. `ff-only` skips the temporary worktree and fast-forwards directly, failing with 409 `not_fast_forward` if the target has moved.
  3. On conflicts the merge is aborted, nothing changes, and the call returns 409 `conflict` with the conflicting paths. The UI offers Resolve with agent, which prompts the session to merge the target into its branch and fix the conflicts; then retry.
  4. Updating the target: if it is not checked out anywhere, `git update-ref refs/heads/<target> <new> <old>` (compare-and-swap). If it is checked out in the main checkout, `git merge --ff-only <new>` runs there, which updates the working tree safely and fails with 409 `target_dirty` if your local changes would be overwritten.
  5. `delete_session: true` runs the delete flow after a successful merge. The branch `agent/<id>` is then deleted because it is merged.
  6. Returns `{merged_commit, target, strategy}`. The hub timeout for merge and publish is 60 seconds (the default stays 5).
- `POST /api/sessions/:id/publish` with `{remote, branch?, confirm:true}`. The server pushes the session branch `agent/<id>` using the host's own git credentials. Rules: only remotes named in `publish.remotes` config (empty by default, so the endpoint is disabled); never force; refuse protected names (main, master, release/\*); `touches_ci` is true when the diff changes `.github/`, other CI files or hook directories, and the UI shows a warning before confirm. Pushing the merged target branch is not supported from the app; push main from the host. Containers still never hold a git credential.
- UI: the Session screen gets a Changes sheet with the file list, per-file diff, ahead and behind counts, a target and strategy picker with a Merge button, a Publish button when configured, and warnings for uncommitted work and CI files.

## F. Operations

### 13. Runaway controls

| Config key | Default | Effect |
| --- | --- | --- |
| `turn_timeout_min` | 45 | Abort the turn using the ladder in item 16 |
| `idle_stop_min` | 30 | Idle session becomes `stopped`, freeing a slot |
| `max_session_tokens` | unset | At the limit, abort and record reason `budget_exceeded` |

Sessions gain `usage_in` and `usage_out`, updated from the `usage` on `message.end`. If pi does not report usage, the token budget is disabled; confirm in the vertical slice.

### 14. Disk

- `min_free_gb` (default 20): session creation and resume fail with 507 below it.
- `session_disk_soft_gb` (default 10): a watchdog runs `du` on each live session's worktree every 5 minutes. Over the limit it emits an `error` event with code `disk_quota`, aborts the turn and stops the session (resumable after cleanup).
- Docker storage limits do not apply to bind mounts. Optional hardening: project quotas on the data filesystem.
- Build and package caches: per-session and isolated (decided in D2). Each session installs and builds from scratch in its own worktree, and no cache directory is shared between sessions. Expect slow first builds. The soft cap above includes target/ and node\_modules, so raise session\_disk\_soft\_gb for large Rust projects.

### 15. Event growth, history and tool output

- Coalescing: inside each 25 ms commit, consecutive deltas of the same kind and message become one row with one seq.
- Snapshots: for messages completed more than 24 hours ago, a compaction job replaces their delta rows with one `message.snapshot` event (full text, thinking, message id) taking the highest seq of the removed rows. Clients apply it by message id, replacing rather than appending, so it is safe to receive after partial deltas. Seq rule change: seq is strictly increasing and gaps are allowed only where compaction removed rows.
- New devices: with no `Last-Event-ID` the stream starts at the beginning of the 50th most recent message and sends `history.truncated {before_seq}`. Older history loads with `GET /api/sessions/:id/events/history?before=<seq>&limit=200`.
- Tool output: the full text goes to `sessions/<id>/tool/<tool_id>.txt` (capped at 5 MB with a truncation marker); `tool.end` carries a truncated `output` plus `output_ref`. Fetch with `GET /api/sessions/:id/tool-output/:tool_id`. The earlier wording about keeping full output in the log is removed; `session.log` holds stderr (the kept tail of it) and the server's own launch line, and never a tool's output.
- Stopped-session retention (decided in D3): a daily job deletes sessions that have been stopped for more than 7 days (config stopped\_ttl\_days, default 7), using the delete flow from item 9. A snapshot runs first; if it fails, the session is skipped and retried the next day. Sessions in error are never auto-deleted. Add stopped\_at INTEGER to sessions, set when the status becomes stopped and cleared on resume. The Sessions screen shows the days remaining on each stopped session.

### 16. Abort ladder, hub gaps

Abort returns 202 immediately; progress arrives as events.

1. `clear_queue`, then `abort` over RPC.
2. If pi is not idle after `abort_grace` (10 s), run `docker stop -t 5 s_<id>`. The session becomes `stopped` with reason `forced_abort`, and the server resumes it at once (the same session id reopens the pi session). `/work` is a host directory, so file changes survive. The turn timeout uses the same ladder.

Hub gaps:

- Preview routing for non-hub machines is in item 3.
- Registry divergence: each machine reads an optional `hosts.toml` at startup and merges it into its hosts table (the file wins on conflicts), so one file copied to every machine keeps the lists aligned. Later option: discover machines by Tailscale tag instead of a registry.

## Data model and API changes

```sql
-- sessions
worktree TEXT NOT NULL,              -- as in the original spec
status TEXT NOT NULL,                -- starting|idle|running|stopped|error|deleting
status_reason TEXT,
client_key TEXT UNIQUE,              -- idempotency key
network INTEGER NOT NULL DEFAULT 0,  -- internet access checkbox
stopped_at INTEGER,                  -- set when status becomes stopped
usage_in INTEGER NOT NULL DEFAULT 0,
usage_out INTEGER NOT NULL DEFAULT 0,

CREATE TABLE push_subscriptions (
  endpoint TEXT PRIMARY KEY,
  p256dh TEXT NOT NULL,
  auth TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
```

| Endpoint | Change |
| --- | --- |
| `POST /api/sessions` | Returns 202, accepts `network` (boolean), honours `Idempotency-Key` |
| `DELETE /api/sessions/:id` | Returns 202, `?force=true` option, 409 `unsaved_work` |
| `POST /api/sessions/:id/abort` | Returns 202 |
| `POST /api/sessions/:id/resume` | New |
| `GET /api/sessions/:id/changes`, `/changes/diff` | New |
| `POST /api/sessions/:id/merge` | New: merge into a target branch on that machine |
| `POST /api/sessions/:id/publish` | New, disabled unless configured |
| `GET /api/sessions/:id/tool-output/:tool_id` | New |
| `GET /api/sessions/:id/events/history` | New |
| `POST` and `DELETE /api/push/subscribe` | New |
| `ANY /<host>/<id>/*` on the preview listener | Replaces `/preview/:id/*` |
| Admin unix socket | `drain` command, not reachable over HTTP |

All non-GET calls also require the JSON content type and `X-Holosmith-Client` header from item 4.

## Decisions made

- **D1. Pi authentication: API keys.** A key proxy keeps the keys out of containers (item 2).
- **D2. Caches: per-session and isolated.** Safest, slower builds, more disk (item 14).
- **D3. Stopped sessions: auto-delete after 7 days.** Work is snapshotted first (item 15).

Spec edits that follow from D1:

- Launch command and container section: replace `-v <pi-agent>:/home/agent/.pi/agent` with `-v <data>/sessions/<id>/pi-home:<the same path>` and set `PI\_CODING\_AGENT\_DIR` to it. The destination is the host path, not `/home/agent/.pi/agent`: the image decides its own user, item 1 mounts the `.git` paths at their host paths for the same reason, and a destination Docker creates is root-owned — so an image running as an unprivileged user could not write its own config directory.
- Security table, row Pi login: tokens are never in containers; the agent can use its session's dummy key only through the key proxy.
- Limits table, row Pi login: one key proxy per machine instead of one shared pi agent directory.
- Open question 2 is closed and replaced by the vertical-slice check for base-URL override in item 2.
- Open question 9 is closed: project trust is approved non-interactively inside the container with pi's `--approve`, per process, never from a saved decision or a global default. The launch line in `session.log` records that decision, and a test that needs a daemon and the agent image proves the mechanism by asking the live agent for its command list and finding the project's own resources there.

## Single binary and CLI

One static binary, `holosmith`, is the hub, the server, the two proxies, the container helpers and the deploy tool. A machine needs only Docker (servers), git (servers) and Tailscale installed.

### Roles

| Role | Runs | Needs |
| --- | --- | --- |
| `hub` | Embedded web app, host registry, request and preview forwarding | Tailscale |
| `server` | Sessions, containers, SQLite, key and egress proxies, previews, push | Docker, git 2.38+, Tailscale |
| `both` | Everything above; the default on a single machine, and any machine can be the hub | All of the above |

Running `holosmith server deploy` and `holosmith hub deploy` on the same machine makes it `both`. A hub-only machine has no Docker and no sessions, so its Computers screen lists only registered servers. API paths are unchanged on every role: a hub serves `/api/hosts*`, a server serves the session API.

### Commands

| Command | What it does |
| --- | --- |
| `holosmith serve [--config PATH]` | Runs in the foreground with the role from the config; systemd runs this |
| `holosmith server deploy [--ssh user@host] [--binary PATH] [--root DIR] [--dry-run]` | Installs and starts a server (steps below); `--root` builds every installed path under `DIR`, as the invoking user |
| `holosmith hub deploy [--ssh user@host] [--binary PATH] [--dry-run]` | Installs and starts a hub |
| `holosmith hub host add <name> <url>`, `list`, `remove <name>` | Manages the registry from the CLI, with the same checks as `POST /api/hosts` |
| `holosmith key set <provider>`, `list`, `remove` | Stores provider API keys for the key proxy; the key is read from stdin, never from arguments |
| `holosmith doctor [--quick or --full]` | Preflight checks and self-tests |
| `holosmith status` | Role, version, uptime, session count, proxies, last health of registered machines |
| `holosmith logs [-f] [--session ID]` | Server log from journald, or a session's `session.log` |
| `holosmith upgrade <binary>` and `holosmith rollback` | The promotion pipeline and rollback from item 10 |
| `holosmith drain [--wait 10m]` | The drain from item 10 |
| `holosmith config show`, `edit`, `validate` | Config handling; validate rejects unknown keys |
| `holosmith uninstall [--purge]` | Removes unit, proxies and networks; `--purge` also removes data |
| `holosmith version` | Version, git tag, pinned pi version, schema version |

Internal subcommands, used inside containers: `holosmith proxy egress`, `holosmith proxy key`, `holosmith container snapshot` (the `hs-snapshot` logic from item 9) and `holosmith container fake-pi` (replays the recorded session for tests).

### What `server deploy` does

It is idempotent: each step checks the current state first, so re-running is safe. `--dry-run` lists the steps
without running them. Each step reports one of `satisfied`, `needs` (a dry run's plan), `changed`, `skipped`
(with the story that still owns that step, never a pass) or `failed`; a run that fails stops at that step,
leaves the steps before it in place and prints the command to re-run, because every step reads the state it
would change.

1. Preflight, as `doctor --quick`: Linux with systemd, Docker reachable, git 2.38 or newer, Tailscale logged in, free disk above `min_free_gb`, ports free.
2. Create the service user `holosmith` (in the docker group), the data directory, `/etc/holosmith/config.toml`, a 0700 secrets directory and the admin socket directory. Generate the VAPID keys.
3. Build two images from Dockerfiles embedded in the binary: `holosmith-proxy:<version>` (the static binary on a minimal base, used for both proxies) and `agent-image:<version>` (pinned pi version and base image digest, git, the toolchains named in the config, and the binary for the helper subcommands).
4. Create the `egress` network and start the `hs_egress` and `hs_key` containers with restart policy `unless-stopped`. Per-session networks are created by the server.
5. Install and start `holosmith.service` (`Restart=on-failure`, `ExecStartPre=holosmith doctor --quick`).
6. Configure `tailscale serve` for the control origin (443) and the preview origin (8444).
7. Wait for `/api/health`, run `doctor --full` (it includes the egress self-test and the network-off probe), then print the URL and next steps: `holosmith key set <provider>` and registering the machine in a hub.

`hub deploy` runs steps 1 (without the Docker and git checks), 2 (without the docker group), 5, 6 and 7, and builds no images or proxies.

With `--ssh`, the CLI copies the same binary to the target, verifies its SHA-256 after the copy, and runs the same deploy there with sudo, streaming the output. It refuses if the target's CPU architecture differs, unless `--binary` supplies a matching build. The CLI itself downloads nothing from the internet.

### Config

One TOML file holds the role, listen addresses, data and projects roots, `docker_host` (for rootless Docker), the hosts seed, `allowed_hubs`, the egress allow-list, and every key introduced in these amendments: `network_default`, `merge.targets`, `publish.remotes`, `project_backup`, `turn_timeout_min`, `idle_stop_min`, `max_session_tokens`, `min_free_gb`, `session_disk_soft_gb`, `stopped_ttl_days`, `abort_grace`. API keys are not in this file: they live in `<data>/secrets/keys.toml` (mode 0600), mounted read-only into `hs_key` only.

### Build and release

- Targets: x86\_64 and aarch64 Linux, static musl, with SQLite bundled. The web app is built first and embedded in the binary.
- `holosmith version` prints the pinned pi version, and `doctor` fails if the agent image's pi differs from it.
- An upgrade rebuilds the two images only when the binary or a pinned version changed.
- Self-development builds the new binary in a builder container (item 10); `holosmith upgrade <path>` takes it from there.
- Assumption (change on request): deploy targets are Linux with systemd. On macOS or Windows you can run `holosmith serve` by hand, with no deploy support, and the Docker Desktop dev-port caveat from the original spec still applies.

### CLI by phase

| Phase | CLI delivered |
| --- | --- |
| 1. Vertical slice | `serve`, `doctor`, `key set`, `version`, `config validate`; the static build runs on both architectures |
| 2. MVP | `server deploy` (local), `status`, `logs`, `drain`, `uninstall`, the internal subcommands |
| 3. Multiple computers | `hub deploy`, `hub host ...`, `--ssh` deploys |
| 4. Self-development | `upgrade`, `rollback` |

## Vertical slice verification

These checks run during phase 1 on a real Docker host, and `holosmith doctor --full` automates as many as it can. A failed check changes the design as stated, before phase 2 builds on it. The **Result** column is filled in by the story that runs the check; HS-046 closes the run.

The **Result** column is the record: there is no second document. A cell carries the verdict, the automation that produced it — a gate case, a cargo test, the build, or the by-hand procedure in `docs/build.md` — and, where a check failed, the fallback taken. `case_verification_table` fails the gate when a row still says "not yet run", when a cell carries no verdict, when a row that says `fail` records no fallback, when a cell names an automation this repository does not have or when the table stops having eleven rows. **HS-070 filled V11, the last deferred row**, so the rule that used to pin that deferral is gone: every row is measured by the same four rules.

Repeatable means one command: `scripts/verify.sh` runs every row's automation except V3–V5 and prints one `ok <case>` line per case, so its output names what each row rests on. Only V3–V5 need something a gate may not hold — a real provider key — so they are run by hand from `docs/build.md`, and their cells say so. `doctor --full` covers what a check can answer without a session: V2 is its `merge` finding, and the other rows are not checks — V1 needs a container with the spec's mounts and a scratch repository, V6, V7 and V9 need a running session, V8 is a fixture replay in `cargo test`, and V10 is the build (ADR-0055).

| # | Check | Pass | If it fails | Result |
| --- | --- | --- | --- | --- |
| V1 | Stacked read-only mounts (item 1) | With the four git mounts, writing to `.git/hooks`, appending to `.git/config` and renaming `.git/hooks` all fail, while `git commit`, creating a branch and `git worktree list` work | Mount only the needed `.git` subpaths writable (objects, refs, logs, `worktrees/<id>`) and the rest read-only; if that still leaks, return to per-session clones | **pass** (HS-046): `case_container_mounts` ran one real container with the spec's four mounts and `--cap-drop ALL` and asserted the three writes — into `.git/hooks`, appending to `.git/config`, renaming `.git/hooks` — fail on the host as well as in the container, while `git commit`, `git branch` and `git worktree list` work, that `id -u` in the container is not 0, and that `mount -o remount,rw /` and `umount /work` are refused; `tests/container.rs` asserts the same against the argument vector the binary builds; no fallback needed |
| V2 | Git version (item 12) | `doctor` finds git 2.38 or newer, and `git merge-tree --write-tree` exits 0 for a clean merge and 1 with paths for a conflict | Check mergeability with a trial merge in a temporary worktree | **pass** (HS-046): `doctor --quick`'s `merge` finding ran `git merge-tree --write-tree` in a throwaway repository on this machine and printed `merge: pass`, its sentence saying the command answered 0 for a clean merge and 1 with the path for a conflict; `case_doctor_quick` asserts that line from the shipped artifact and `tests/doctor.rs` (the passing arm on the machine's own git, the refusing arms over a git shim); no fallback needed — the trial-merge fallback stays unimplemented, because the git check refuses below 2.38 and an older git is a `git` finding (ADR-0054) |
| V3 | Key proxy path (item 2) | A real turn streams text and thinking deltas, round-trips a tool call and reports usage, all through the key proxy with a dummy key | Per-session spend-limited key passed as an environment variable | **pass** (HS-040, ADR-0050): a real session answered with text deltas, 434 characters of thinking deltas at `--thinking max` in the recorded run, one `bash` round trip and `usage` on `message.end`, through the proxy, with only the dummy key in the container; no fallback needed. The run first failed — the proxy forwarded a `Content-Length` with no body — and the defect is fixed here; not re-measured in this story — this machine holds no provider key, and no gate may hold one — so this cell quotes HS-040's by-hand run, whose key-free half is `case_key_proxy_request`; to re-measure, follow the credential section in `docs/build.md` |
| V4 | Model listing | `get_available_models` lists the built-in models with only a dummy key and `PI_OFFLINE=1` | Drop `PI_OFFLINE` and allow-list the catalog host, or list models explicitly in `models.json` | **pass** (HS-040): `get_available_models` inside the session's own container answered 29 models for `opencode-go` from pi's bundled catalog, with the dummy key alone and `PI_OFFLINE=1`, so that catalog carries the provider and no `models` array was needed; no fallback needed; not re-measured in this story, because this machine holds no provider key — this quotes HS-040's run, and the credential section in `docs/build.md` is how to repeat it |
| V5 | Credential precedence | With the seeded pi-home carrying no `auth.json` and the container environment no provider key, the key proxy log shows the `models.json` dummy key in use — while a competing `auth.json` reaches the proxy as `bad-key`, which is the order the fallback column exists for | Remove the other sources from the seeded pi-home and the container environment | **pass, with the fallback applied** (HS-040, ADR-0050): measured precedence is `auth.json` > `models.json` `apiKey` > the provider env var, so a competing `auth.json` reaches the proxy as `status=bad-key` and the pass condition holds only while the seeded pi-home has no `auth.json` and the container environment has no provider key — which is what the build does; the fallback is therefore the rule, not a repair; not re-measured in this story, because this machine holds no provider key — this quotes HS-040's run, and the credential section in `docs/build.md` is how to repeat it |
| V6 | Egress and the network checkbox | A direct connection from a container fails; through the proxy a non-listed host gets 403; with the checkbox off only the key proxy answers | Fix the network wiring before anything else proceeds | **pass** (HS-046): `case_internet_toggle` created two real sessions that differ only in the body's `network` flag and asked two questions from a probe container on each session's own network — a direct connection to the internet was unreachable in both, and the `egress-proxy` alias did not resolve with the flag off while it answered 403 for a host that is not on the allow-list with it on — then compared the two containers with `docker inspect` (every environment variable identical once each session's id is normalised) and asserted the flag as the network's endpoint count after detaching the proxy, killing the container and resuming it: 1 for the off session and 2 for the on one; no fallback needed |
| V7 | Host to container reach (item 3) | The host can fetch from a stub HTTP server inside a container on an internal session network | Run the preview proxy as a container on the session network | **pass** (HS-041, ADR-0051; run again by HS-046): the host fetched `http://172.18.0.2:8099/` from a container on `--internal` `hs_v7probe` while that container reached neither the internet nor the host, and `case_preview`'s second half does the same on a session's own `agents_<id>` network — the host's direct fetch answered 200, the fetch through the shipped artifact's `/local/<id>/` answered 200 with the container's own bytes, and a WebSocket handshake and echo went both ways; the listener proxies in-process and the fallback container is not implemented |
| V8 | Pi event shapes | Recorded sessions cover an image in a tool result, usage on `message_end`, and a project-local extension asking for UI input in RPC mode; the mapping layer handles all three | Add handlers; for UI requests, auto-cancel and emit an `error` event | **pass** (HS-046): the five recorded turns in `crates/holosmith/tests/fixtures/` replay through `tests/rpc.rs` — an image in a tool result (stored as a file and named additively on `tool.end`), usage on `message.end` (input plus the cache counters, with `in + out == totalTokens` asserted on every record of every committed turn), and a project-local extension asking for UI input in RPC mode, which the pump answers with `extension_ui_response` before the frame and maps to `turn.error`; a record this build has never seen is a `warn` and a known-but-unwanted one a `declined` count; no fallback needed |
| V9 | Crash safety (items 5 and 6) | `kill -9` of the server mid-turn, then restart: the session returns to idle with no duplicate or missing seq and no container name conflict | Fix before phase 2 | **pass** (HS-044, ADR-0053): a `kill -9` of the served artifact mid-turn and a restart left the row `idle · interrupted`, `sessions.last_seq` equal to the highest committed seq with the log's seqs unique and gap-free, and the restarted session's container back under `s_<id>` with a new id — the same run removed an orphan container, network and worktree directory while keeping the branch and the snapshot ref. HS-046 ran the set: `case_reconcile` (neither daemon nor image, the row half) and `case_reconcile_orphans` (the daemon and the image, the name half) |
| V10 | Static binary | The musl build starts on x86\_64 and aarch64, serves the embedded web app and opens SQLite | Adjust the build until it does | **pass** (HS-046): `scripts/build.sh` built both musl targets and `scripts/check-static.sh` proved each has no dynamic section and carries the embedded web app and the embedded migrations, then ran each artifact in an empty root — x86_64 natively and aarch64 under `qemu-aarch64-static`, each printing `runs with no loader and no userspace dependencies` and serving `web/dist/index.html` and its SQLite database; the same artifact served this session's gate; no fallback needed, and a machine without the emulator prints one `SKIP` line and still passes |
| V11 | Hub identity (phase 3) | The `allowed_hubs` whois check accepts the hub and rejects another tailnet node | Fall back to ACLs only and record it as a risk | **pass, with the fallback applied** (HS-070, ADR-0075): `case_machine_trust` booted one machine twice and drove every request from a client bound to `127.0.0.2` against a `tailscale` shim on `PATH` that records every argv — the empty list served the stranger and asked no whois at all (the ACLs-only fallback), `allowed_hubs = ["workshop"]` served the node the whois named `workshop` (and the shim's own `whois --json 127.0.0.2` is the proof the **address** was verified rather than a header), another node earned `403 peer_not_allowed` with exactly one logged line naming the peer and no configured entry anywhere in the answer, a forged `Tailscale-User-Login: workshop` changed nothing, an unreadable identity earned `403 peer_unverifiable` without repeating the daemon's prose, `127.0.0.1` was served with no whois (the front door), and the **preview** hop refused the same stranger in plain text while letting a listed peer through to its own `404`; `tests/trust.rs` drives every arm in-process plus the decision table. **The fallback is applied because of the topology, and ADR-0075 D10 records it:** `server deploy` step 6 publishes the control origin with `tailscale serve --https=443 http://127.0.0.1:<port>`, so on a deployed machine a forwarded call arrives from `127.0.0.1` — the front door — and the check that bites there is the tailnet's ACLs plus item 4's `Tailscale-User-Login`/`owner` rule; the whois check bites for a machine whose control origin is its listener's own tailnet-reachable address (and for any caller dialling the listener directly), which is what the gate measures. A machine-identity path **behind** `tailscale serve` is not this story's and is named as a follow-up in ADR-0075 |

## Known and accepted risks

These are accepted for v1. Each row says what limits it.

| Risk | Why accepted | What limits it |
| --- | --- | --- |
| Shared `.git` across worktrees: the agent can rewrite or delete branches (including main) and damage objects or other sessions' worktree metadata | Worktrees were chosen over per-session clones | Hooks and config mounted read-only; optional daily bundle backup; keep a remote |
| Agent-written code runs on your host when you build, test or run it (`build.rs`, scripts, editor tasks, `.envrc`) | Inherent to using agent output | Diff review before merge; promotion builds run in a container; merge skips hooks |
| With Internet access on, project code can leave through allow-listed hosts, because HTTPS is opaque to the proxy | Packages need registries | The per-session checkbox; minimal allow-list; request log |
| An agent can spend your API quota while its session lives, using its dummy key through the key proxy | Needed for the agent to work | Turn timeout, idle stop, token budget, key revoked on delete |
| Prompt injection from project files or fetched content can steer the agent | Cannot be eliminated | Container limits; turn Internet access off for untrusted projects |
| Pi project trust is approved inside the container (open question 9): the launch passes `--approve`, so a project's own `.pi` extensions and prompts load and execute | RPC mode cannot ask, and a project that needs its extensions is the normal case | Container isolation and the key proxy (an extension runs with the agent's dropped capabilities); revisit for untrusted repositories |
| No in-app login: any device that Tailscale ACLs allow can start containers | One-user tailnet | ACLs, Host, Origin and header checks, Tailscale identity check |
| Previews of different sessions share one preview origin | All previews are yours | Kept separate from the control origin |
| Containers share the host kernel, so an escape needs a kernel bug | No gVisor or Kata in v1 | Non-root, cap-drop ALL, no-new-privileges, no Docker socket, resource limits |
| The hub is a single entry point that can reach every machine | By design | The allowed\_hubs check; ACLs |
| Events, tool output and images are stored unencrypted on disk, including any secrets that appear in output | Single-owner machine | Disk permissions; optional disk encryption |
| Server-side publish uses host git credentials on a phone tap | Convenient shipping | Off by default; named remotes only; no force; CI-file warning; confirm step |
| The service user is in the docker group, which is root-equivalent on the host, so a compromised server process is a compromised host | Controlling Docker through its CLI needs it; rootless Docker is possible through docker\_host | Dedicated user, Tailnet-only listener, input validation, no Docker socket in containers |
| Deploying over SSH copies a binary to the target and runs it with sudo | You deploy to your own machines | SHA-256 verified after the copy; --dry-run; the CLI downloads nothing itself |
| Image builds pull base images, pi and toolchains from public registries | Needed to build the agent image | Pinned pi version and base image digests; images rebuilt only on upgrade |
| Machine trust is off when `allowed_hubs` is empty, and a machine that lists hubs can verify nobody where `tailscaled` does not answer | The list is the operator's own setting and its default is empty, so a fresh machine keeps working; a whois that cannot be read is a refusal and never an admission, because an unread identity is not an identity | Tailscale's ACLs on every node (V11's documented fallback), `doctor --full`'s Tailscale finding, and one `warn` per refusal naming the peer and the reason — `allowed_hubs` is left empty only where the tailnet's ACLs already restrict who can reach the machine. **The topology half:** `server deploy` step 6 puts `tailscale serve` in front of the control listener with a loopback target, so on a deployed machine every caller — a browser, the hub, a stray tailnet node — arrives from `127.0.0.1` and the `allowed_hubs` whois check has no address to verify; there the effective control is the tailnet's ACLs plus item 4's `owner` check, and the whois check bites only where the control origin is the listener's own tailnet-reachable address (ADR-0075 D10) |
