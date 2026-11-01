# Phase 1b — The real Docker backend

**Goal:** swap the stub for real containers, real worktrees, real pi, real network isolation and real credentials — without changing the API, the event contract or the UI.

**Exit criteria:** every flow from phase 1 works against a real Docker host with real pi; a container can reach nothing except the key proxy (and allow-listed hosts when Internet access is on); the agent holds only a dummy key; a preview is served from a separate origin; and V1–V10 have been run and recorded, with any resulting design change written down.

Nothing in this phase may require a UI or API change. If it does, that is a signal the phase-1 contract was wrong and the change belongs in a story of its own.

See ADR-0001 (why the stub came first) and ADR-0003 (why the V-checks are gates here).

---

### HS-024 — Backend selection and Docker preflight

**As** the operator **I want** the binary to tell me up front whether this machine can run sessions **so that** I fail at startup instead of mid-turn.

- **Size:** S · **Blocked by:** HS-023 · **Spec:** Roles, item 2, item 10 (doctor)
- **Demo:** with Docker stopped, creating a session fails with one clear message; with it running, the same call succeeds.

**Acceptance**
- [ ] The backend is chosen by config (`backend = "stub" | "docker"`) and `--backend`.
- [ ] Docker preflight checks: daemon reachable at `docker_host` (rootless supported), git ≥ 2.38, free disk ≥ `min_free_gb`.
- [ ] Failures are actionable single sentences naming the fix, and are reused verbatim by `doctor` (HS-045).
- [ ] The Docker Desktop dev-port caveat is documented where the operator will hit it.
- [ ] Stub and Docker backends pass the same conformance suite; the suite is the definition of "the backend works".

---

### HS-025 — Worktree provisioning

**As** the operator **I want** each session to get its own branch and worktree **so that** parallel agents never fight over files.

- **Size:** M · **Blocked by:** HS-024 · **Spec:** item 1
- **Demo:** create a session, see `.worktrees/<id>` with branch `agent/<id>`, commit in it, see `ahead` increment, delete and see it gone.

**Acceptance**
- [ ] Provision runs `git worktree add -b agent/<id> <projects>/.worktrees/<id> HEAD` and records the worktree path and `base_commit`.
- [ ] `ahead` is computed as `git rev-list --count <base_commit>..agent/<id>`.
- [ ] Teardown runs `git worktree remove --force`, `git branch -D agent/<id>`, `git worktree prune`; a missing worktree is not an error.
- [ ] Orphaned worktrees from a crashed run are pruned at startup (HS-044).
- [ ] No fetch step anywhere: the branch and its refs already live in the project repository.

---

### HS-026 — Worktree refusal rules

**As** the operator **I want** unsafe repositories rejected at creation **so that** I do not discover the hole after an agent exploits it.

- **Size:** S · **Blocked by:** HS-025 · **Spec:** item 1
- **Demo:** a repo with submodules, a `.git` file instead of a directory, and one with `extensions.worktreeConfig` each return 422 with a distinct code.

**Acceptance**
- [ ] 422 `git_not_directory` when the project's `.git` is not a directory.
- [ ] 422 `submodules_present` when submodules exist (their hooks live under `.git/modules`).
- [ ] 422 `worktree_config_enabled` when `extensions.worktreeConfig` is on.
- [ ] Checks run before `git worktree add`, so a refused session leaves nothing behind.
- [ ] The UI explains the refusal in one sentence rather than showing a code.

---

### HS-027 — Hardened `.git` mounts and V1

**As** the operator **I want** the agent to be unable to plant anything that runs on my host **so that** a container escape needs a kernel bug, not a git hook.

- **Size:** M · **Blocked by:** HS-026 · **Spec:** item 1, V1
- **Demo:** inside the container, writing `.git/hooks/*`, appending `.git/config` and renaming `.git/hooks` all fail; `git commit`, `git branch` and `git worktree list` all work.

**Acceptance**
- [ ] Mounts exactly as specified: worktree at `/work`, `<project>/.git` at its host path, `.git/hooks` read-only, `.git/config` read-only.
- [ ] Host paths are identical inside the container, because the worktree's `.git` file points at `<project>/.git/worktrees/<id>`.
- [ ] `--cap-drop ALL`, `--security-opt no-new-privileges`, non-root user; the container cannot remount or unmount.
- [ ] **V1 gate:** a scripted check asserts all six outcomes above.
- [ ] If V1 fails, the fallback from the spec is applied (writable objects/refs/logs/worktrees only, rest read-only), re-verified, and recorded as an ADR with the outcome.

---

### HS-028 — Container run with limits

**As** the operator **I want** one container per session with hard limits **so that** a runaway agent cannot take down the machine.

- **Size:** S · **Blocked by:** HS-027 · **Spec:** item 6, Known risks (resource limits)
- **Demo:** `docker inspect` shows dropped capabilities, no-new-privileges, a memory and CPU limit, and no Docker socket.

**Acceptance**
- [ ] `docker run -i --rm --init --name s_<id> --network agents_<id>` with the four git mounts and `-e HS_SESSION_ID=<id>`.
- [ ] CPU, memory and PID limits come from config with documented defaults.
- [ ] No Docker socket, no host network, no privileged, no added capabilities.
- [ ] The container's working directory is `/work`; the process is not root.
- [ ] A conformance test asserts each flag is actually present on the created container.

---

### HS-029 — Name conflicts and forced removal

**As** the operator **I want** a restart never to fail on a leftover name **so that** recovery is boring.

- **Size:** S · **Blocked by:** HS-028 · **Spec:** item 6
- **Demo:** kill a container out from under the server, start the session again, no name conflict.

**Acceptance**
- [ ] Before every start, `docker rm -f s_<id>` runs and not-found is ignored.
- [ ] `--rm` means a stopped container never leaves its name behind.
- [ ] Session data lives in host directories, so removing the container loses nothing — asserted by a test that stops, removes and resumes.
- [ ] Two starts for the same session cannot race: a per-session lock serialises them.

---

### HS-030 — Exit classification

**As** the operator **I want** the UI to distinguish "I stopped it" from "it died" **so that** I know when something is actually wrong.

- **Size:** S · **Blocked by:** HS-029 · **Spec:** item 6, item 8
- **Demo:** three runs produce `stopped` (expected stop), `stopped` with reason `pi_exit` (exit 0), and `error` with the last stderr lines.

**Acceptance**
- [ ] The expected-stop flag is persisted before every `docker stop -t 10`.
- [ ] Exit with the flag set → `stopped`.
- [ ] Exit 0 without the flag → `stopped` with reason `pi_exit`.
- [ ] Any other exit → `error` with the last stderr lines as `status_reason`; the lines are also kept in `session.log`.
- [ ] The classifier is a pure function with unit tests for all three cases and the flag-cleared case.

---

### HS-031 — Real pi launch, ready handshake and project trust

**As** the operator **I want** a real agent running per session **so that** the product does actual work.

- **Size:** L · **Blocked by:** HS-030 · **Spec:** item 2 (launch), V8 (trust), open question 9
- **Demo:** create a session against a real project; the container starts pi, reports ready, and the session is `idle` and accepts a prompt.

**Acceptance**
- [ ] Per-session agent directory at `<data>/sessions/<id>/pi-home`, seeded from a template with model preferences and no secrets.
- [ ] Launch environment: `PI_CODING_AGENT_DIR` at that directory, `NO_PROXY=key-proxy`, `PI_OFFLINE=1`, `PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0`.
- [ ] pi runs in RPC mode over stdio; "ready" is detected from a real signal, not a sleep, and moves `starting → idle`.
- [ ] The agent image's pi version equals the binary's pinned pi version; a mismatch fails the session with a clear error.
- [ ] **Open question 9 resolved:** project trust is approved non-interactively inside the container; the mechanism is implemented, documented in the spec, and flagged in the risk table.
- [ ] The launch line is recorded in `session.log` (redacted) so a failure is diagnosable.

---

### HS-032 — RPC mapping: message deltas

**As** the operator **I want** to see the agent's text and thinking stream in **so that** the phone experience is live rather than a wait.

- **Size:** M · **Blocked by:** HS-031 · **Spec:** item 15, V8
- **Demo:** a real turn streams text and thinking into the browser, in order, with no duplication.

**Acceptance**
- [ ] Text and thinking deltas map to events tagged with message id and kind.
- [ ] Every delta reaches the writer; ordering is preserved end to end.
- [ ] A completed message produces `message.end` with usage, tool-call summary and stop reason.
- [ ] Malformed or unknown RPC frames are logged and skipped, never fatal.
- [ ] A recorded fixture (real frames captured from a real turn) is committed for replay tests.

---

### HS-033 — RPC mapping: tool calls and output files

**As** the operator **I want** to see what the agent ran without bloating my database **so that** history stays fast and output stays inspectable.

- **Size:** M · **Blocked by:** HS-032 · **Spec:** item 15
- **Demo:** a tool that prints 2 MB shows a short preview in the transcript and a link that fetches the full text.

**Acceptance**
- [ ] Tool start and end events carry name, arguments summary, exit status and duration.
- [ ] Full output goes to `sessions/<id>/tool/<tool_id>.txt`, capped at 5 MB with a truncation marker.
- [ ] `tool.end` carries a truncated `output` plus `output_ref`.
- [ ] `GET /api/sessions/:id/tool-output/:tool_id` returns the file; path traversal is impossible.
- [ ] `session.log` holds stderr only — the earlier "full output in the log" behaviour is not implemented.

---

### HS-034 — RPC mapping: usage

**As** the operator **I want** real token counts **so that** the budget control is not theatre.

- **Size:** S · **Blocked by:** HS-032 · **Spec:** item 13, V3
- **Demo:** a real turn updates `usage_in`/`usage_out`; setting `max_session_tokens` below it stops the next turn.

**Acceptance**
- [ ] Usage on `message.end` is persisted to `usage_in`/`usage_out` and emitted as an event.
- [ ] Missing usage leaves the budget disabled and surfaces a visible warning once per session.
- [ ] HS-023's budget path is exercised against the real backend, not only the stub.
- [ ] Counts survive stop and resume.

---

### HS-035 — RPC mapping: images and extension UI requests (V8)

**As** the operator **I want** the two awkward real-world frames handled **so that** a session does not hang on something the mapping layer never saw.

- **Size:** M · **Blocked by:** HS-033 · **Spec:** V8
- **Demo:** a tool result containing an image renders (or links) correctly; an extension asking for UI input auto-cancels and produces an `error` event instead of a hung turn.

**Acceptance**
- [ ] Image content in a tool result is stored and rendered or linked; it never corrupts the transcript.
- [ ] A project-local extension requesting UI input in RPC mode is auto-cancelled and emits an `error` event with a clear reason.
- [ ] Recorded fixtures cover all three V8 cases and replay in the test suite.
- [ ] **V8 gate:** the three recorded sessions are replayed and pass.

---

### HS-036 — Per-session network and the startup self-test

**As** the operator **I want** containers that cannot reach the internet or my host directly **so that** isolation is enforced by the kernel, not by an environment variable.

- **Size:** M · **Blocked by:** HS-028 · **Spec:** item 2, V6
- **Demo:** a container on its session network cannot reach `1.1.1.1` or the host, and the server refuses to start sessions if it can.

**Acceptance**
- [ ] `docker network create --internal agents_<id>` per session; the container joins only that network.
- [ ] The `egress` network exists and holds the proxies.
- [ ] A startup self-test creates a temporary internal network and a probe container; a direct connection to the internet or to the host must fail, or the server refuses to start sessions.
- [ ] Orphaned `agents_*` networks are removed at startup (HS-044).
- [ ] Network creation failure rolls the session back to `error` without leaving a network.

---

### HS-037 — Egress proxy

**As** the operator **I want** the only route out to be an allow-listed, logged proxy **so that** exfiltration needs a host I deliberately permitted.

- **Size:** L · **Blocked by:** HS-036 · **Spec:** item 2, V6
- **Demo:** `curl` to an allow-listed host succeeds; a non-listed host gets 403; both are logged with the session id.

**Acceptance**
- [ ] Runs as the `hs_egress` container on the `egress` network, connected to each session network; restart policy `unless-stopped`.
- [ ] Accepts `CONNECT` to port 443 only; anything else is refused.
- [ ] Only allow-listed domains are permitted; everything else gets 403.
- [ ] Any destination resolving to loopback, link-local, RFC 1918 or 100.64.0.0/10 is refused regardless of the allow-list.
- [ ] Logs host and bytes per request, using the session id as proxy username for attribution.
- [ ] Provider hosts are **not** on the allow-list — the key proxy handles those.
- [ ] Containers get `HTTPS_PROXY=http://egress-proxy:3128` and resolve nothing externally.

---

### HS-038 — Internet access checkbox wiring (V6)

**As** the operator **I want** Internet access to actually change reachability **so that** "off" means off.

- **Size:** M · **Blocked by:** HS-037 · **Spec:** item 2, V6
- **Demo:** with the box off, `npm install` fails instantly and only the key proxy answers; with it on, allow-listed registries work.

**Acceptance**
- [ ] `network=false`: the egress proxy is not connected to the session network; only the key proxy is reachable.
- [ ] `network=true`: the egress proxy is connected and the allow-list applies.
- [ ] Proxy environment variables are identical in both cases — the difference is topology, not configuration.
- [ ] The choice is stored on the session, so a resume keeps it, and is visible in the UI.
- [ ] The startup self-test also covers the off case: on a session network without the egress proxy, nothing but the key proxy answers.
- [ ] **V6 gate:** `curl --noproxy '*'` fails; a non-listed host through the proxy returns 403; the off case passes.

---

### HS-039 — Key proxy

**As** the operator **I want** my API keys never to enter a container **so that** a compromised agent cannot exfiltrate them.

- **Size:** L · **Blocked by:** HS-038 · **Spec:** item 2, D1, CLI phase 1
- **Demo:** `holosmith key set <provider>` then a real turn works, while `grep -r <key>` across the container finds nothing.

**Acceptance**
- [ ] Keys live in `<data>/secrets/keys.toml` mode 0600, mounted read-only into `hs_key` only.
- [ ] `holosmith key set <provider>` reads the key from stdin only, never from arguments; `key list` shows providers, never values; `key remove` exists.
- [ ] Each session gets a random dummy key; `models.json` in its pi-home sets `baseUrl` to `http://key-proxy:8080/<provider>` and that dummy key.
- [ ] The proxy validates the dummy key, swaps in the real key, forwards over HTTPS, and logs requests and bytes per session.
- [ ] The proxy accepts connections only from session networks and forwards only to provider hosts.
- [ ] Deleting a session revokes its dummy key (the HS-019 hook is filled in).
- [ ] Keys are never written to a pi agent directory, an environment variable, or any session mount — asserted by a test that greps the container and its mounts.

---

### HS-040 — Key proxy verification (V3, V4, V5)

**As** the operator **I want** proof that the credential path actually works **so that** I do not build four phases on an untested assumption.

- **Size:** M · **Blocked by:** HS-039 · **Spec:** V3, V4, V5
- **Demo:** a recorded checklist showing a real turn, a model listing and a precedence test all passing through the key proxy.

**Acceptance**
- [ ] **V3:** a real turn streams text and thinking, round-trips a tool call and reports usage, all with a dummy key.
- [ ] **V4:** `get_available_models` lists the built-in models with only a dummy key and `PI_OFFLINE=1`.
- [ ] **V5:** with conflicting values in `models.json`, `auth.json` and the provider env var, the proxy log shows the `models.json` dummy key in use.
- [ ] Each failure has its documented fallback applied and recorded: spend-limited key via env var (V3), drop `PI_OFFLINE` or allow-list the catalog host (V4), strip the other sources from the seeded pi-home (V5).
- [ ] Results are written into the spec's verification table.

---

### HS-041 — Preview listener (V7)

**As** the operator **I want** to open a session's dev server from my phone **so that** I can look at what the agent built.

- **Size:** M · **Blocked by:** HS-038 · **Spec:** item 3, V7
- **Demo:** a stub HTTP server in a container is reachable at `http://<preview-origin>/local/<id>/` from another machine, and a WebSocket through it works.

**Acceptance**
- [ ] A second listener serves only `/<host>/<id>/*` (HTTP and WebSocket) and has no API routes at all.
- [ ] WebSocket upgrades check that `Origin` is the preview origin.
- [ ] Local sessions use the host name `local`; the route is forward-compatible with hub routing (HS-069).
- [ ] The session screen links to the preview URL for its origin.
- [ ] **V7 gate:** the host can fetch from a stub HTTP server inside a container on an internal session network; if it cannot, the documented fallback is implemented — the preview proxy runs as a container on the session network — and recorded.

---

### HS-042 — WIP snapshot helper

**As** the operator **I want** uncommitted work captured automatically **so that** a stop or delete never loses it.

- **Size:** M · **Blocked by:** HS-025 · **Spec:** item 9
- **Demo:** make an uncommitted change, settle a turn, and find `refs/holosmith/wip/<id>` pointing at it while the branch and working tree are untouched.

**Acceptance**
- [ ] `holosmith container snapshot` implements the spec script: temp index, `read-tree HEAD`, `add -A`, `write-tree`, commit-tree with the holosmith identity, `update-ref` to `refs/holosmith/wip/<id>`.
- [ ] When the tree equals HEAD's tree the ref is deleted instead of written.
- [ ] The branch, the working tree and the index are never modified.
- [ ] It runs at every `agent_settled`, before any stop, and before delete.
- [ ] Snapshot failure is reported, not swallowed — HS-019 and HS-043 depend on knowing.

---

### HS-043 — Delete hardening and what is kept

**As** the operator **I want** delete to keep whatever is worth keeping **so that** "delete" is not a synonym for "lose".

- **Size:** M · **Blocked by:** HS-042 · **Spec:** item 9
- **Demo:** delete a session with unmerged commits and an uncommitted change; the branch and the wip ref are still there and the recovery command is printed.

**Acceptance**
- [ ] Snapshot runs before delete; failure → 409 `unsaved_work` unless `?force=true`.
- [ ] Branch `agent/<id>` is kept when it has commits beyond base and is not merged into a target branch.
- [ ] `refs/holosmith/wip/<id>` is kept when it differs from the branch.
- [ ] The delete response and the UI show the recovery command: `git branch rescue refs/holosmith/wip/<id>`.
- [ ] Worktree, session directory and network are removed; the row is deleted last.

---

### HS-044 — Startup reconcile and crash safety (V9)

**As** the operator **I want** a crash to leave nothing orphaned **so that** restarting is always safe.

- **Size:** M · **Blocked by:** HS-043 · **Spec:** item 6, item 7, item 8, V9
- **Demo:** `kill -9` mid-turn, restart, and the session is `idle` with the turn marked `interrupted`, no duplicate seq, no name conflict, no orphan network.

**Acceptance**
- [ ] `docker ps -a --filter name=s_` → remove any leftover container, then restart sessions that were live.
- [ ] Rows stuck in `starting` are either resumed or set to `error`; never left dangling.
- [ ] Orphaned `agents_*` networks and `.worktrees/*` directories are removed.
- [ ] A turn that was running is recorded as `interrupted` (a reason, not a state).
- [ ] **V9 gate:** the kill -9 scenario passes with no duplicate or missing seq and no name conflict.

---

### HS-045 — `doctor --quick` and `doctor --full`

**As** the operator **I want** one command that tells me whether this machine is fit **so that** I diagnose once instead of five times.

- **Size:** M · **Blocked by:** HS-044 · **Spec:** CLI, item 10 (step 1 and 7), V2
- **Demo:** `doctor --quick` lists each check with pass/fail; `doctor --full` additionally runs the egress self-test and the network-off probe.

**Acceptance**
- [ ] `--quick`: Linux with systemd where applicable, Docker reachable, git ≥ 2.38, Tailscale logged in, free disk ≥ `min_free_gb`, required ports free.
- [ ] `--full` adds: egress self-test, network-off probe, `hs_egress`/`hs_key` up, agent image pi version equals the pinned pi version.
- [ ] **V2 gate:** `git merge-tree --write-tree` exits 0 for a clean merge and 1 with paths for a conflict; if the git version lacks it, the trial-merge fallback is implemented.
- [ ] Exit non-zero on any failure, with one actionable line per failure.
- [ ] `ExecStartPre` uses `--quick`, so a broken machine does not start serving sessions.

---

### HS-046 — Phase-1 verification run

**As** the maintainer **I want** the whole verification table executed and recorded **so that** phase 2 builds on facts, not hopes.

- **Size:** M · **Blocked by:** HS-045, HS-040, HS-041, HS-035 · **Spec:** Vertical slice verification, item 10
- **Demo:** a filled-in results table in the repo, with every failure and its fallback decision.

**Acceptance**
- [ ] V1–V10 are each run on a real Docker host; V11 is deferred to phase 3 and marked so.
- [ ] Each row records pass/fail, the evidence, and — on failure — which fallback was taken.
- [ ] Any design change is written back into the spec, and any hard-to-reverse change gets an ADR.
- [ ] `doctor --full` automates as many checks as it can, so the run is repeatable rather than a ceremony.
- [ ] This story is the gate for phase 2: phase 2 does not start until it is closed.
