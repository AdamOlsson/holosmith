# Phase 2 — Deployable MVP

**Goal:** the system you actually live with on one machine: installed by one command, reviewable, mergeable, shippable, notifiable, and unable to fill your disk or your database.

**Exit criteria:** `holosmith server deploy` on a fresh host gives a working HTTPS URL; from a phone you can start a session, review its diff, merge it into a target branch, and optionally publish; you are notified when a long turn finishes; disk floors, session quotas, event compaction and stopped-session expiry are all running.

---

### HS-047 — `holosmith server deploy` (local)

**As** the operator **I want** one idempotent command to install a server **so that** standing up a machine is not a checklist I get wrong.

- **Size:** L · **Blocked by:** HS-046 · **Spec:** CLI, What `server deploy` does
- **Demo:** run it twice on a fresh host; the second run changes nothing and reports "already satisfied" per step.

**Acceptance**
- [ ] Runs steps 1–7 from the spec in order, each checking current state first.
- [ ] `--dry-run` lists the steps and their planned actions without doing anything.
- [ ] Failure stops at the failing step, leaves the machine in a describable state, and prints the resume command.
- [ ] Ends by printing the URL and the next steps: `holosmith key set <provider>` and hub registration.
- [ ] Runs as the invoking user with sudo where needed, never assumes it is root.

---

### HS-048 — Embedded Dockerfiles and image builds

**As** the operator **I want** the images to come from the binary **so that** there is no separate artifact to keep in sync.

- **Size:** M · **Blocked by:** HS-047 · **Spec:** What `server deploy` does (step 3), Build and release
- **Demo:** a fresh host ends up with `holosmith-proxy:<version>` and `agent-image:<version>`, both pinned.

**Acceptance**
- [ ] Both Dockerfiles are embedded; `holosmith-proxy:<version>` is the static binary on a minimal base and serves both proxies.
- [ ] `agent-image:<version>` pins the pi version and base image digest, and contains git, the configured toolchains, and the binary for helper subcommands.
- [ ] Image builds are skipped when the binary and pinned versions are unchanged.
- [ ] The image labels carry version, git tag and pinned pi version, so `doctor` can compare them.
- [ ] A build failure names the layer and does not leave a half-tagged image.

---

### HS-049 — Service install, secrets, VAPID and Tailscale origins

**As** the operator **I want** the service to start on boot and be reachable over HTTPS **so that** the phone experience exists.

- **Size:** M · **Blocked by:** HS-048 · **Spec:** What `server deploy` does (steps 2, 5, 6)
- **Demo:** reboot the host; the app answers at the tailnet control origin and previews answer on the preview origin.

**Acceptance**
- [ ] Creates the `holosmith` service user (in the docker group), the data directory, `/etc/holosmith/config.toml`, a 0700 secrets directory and the admin socket directory.
- [ ] Generates VAPID keys at first start if absent and never regenerates them.
- [ ] Installs and starts `holosmith.service` with `Restart=on-failure` and `ExecStartPre=holosmith doctor --quick`.
- [ ] Configures `tailscale serve` for the control origin on 443 and the preview origin on 8444.
- [ ] Ownership and modes are asserted (secrets 0700, keys file 0600); a wrong mode fails `doctor`.

---

### HS-050 — `status`, `logs`, `uninstall`

**As** the operator **I want** to see and control the service from the CLI **so that** I do not need to remember systemd incantations.

- **Size:** S · **Blocked by:** HS-049 · **Spec:** Commands table
- **Demo:** `status` prints role, version, uptime, session counts, proxy health and last health of registered machines; `logs -f` streams.

**Acceptance**
- [ ] `holosmith status` reports role, version, uptime, live/stopped session counts, proxy state, and last health per registered machine.
- [ ] `holosmith logs [-f] [--session ID]` reads journald, or a session's `session.log` when `--session` is given.
- [ ] `holosmith uninstall` removes the unit, proxies and networks but keeps data; `--purge` also removes data.
- [ ] Uninstall refuses to purge while sessions are live unless `--force`.

---

### HS-051 — Drain over the admin socket

**As** the operator **I want** to drain before restarting **so that** an upgrade does not kill a turn I am watching.

- **Size:** M · **Blocked by:** HS-049 · **Spec:** item 10 (step 3), item 8, API table
- **Demo:** start a turn, run `holosmith drain --wait 1m`, and see creates refused with 503 `draining` until the turn finishes.

**Acceptance**
- [ ] `holosmith drain [--wait 10m]` talks to a local unix socket, not HTTP; the command is unreachable over the network.
- [ ] While draining, creates and prompts return 503 `draining`; reads still work.
- [ ] It waits for running turns, then stops idle sessions with reason `server_restart`.
- [ ] `--wait` expiry returns non-zero and leaves the drain state active rather than silently succeeding.
- [ ] Sessions live at drain time return to `idle` on the next startup.

---

### HS-052 — Changes view: summary

**As** the operator **I want** to see what a session changed and whether it can merge **so that** I can decide without opening a terminal.

- **Size:** M · **Blocked by:** HS-046 · **Spec:** item 12
- **Demo:** `GET /api/sessions/:id/changes` shows branch, ahead/behind, dirty, mergeable, per-file line counts and `touches_ci`.

**Acceptance**
- [ ] Returns `{branch, base_commit, head, target, ahead, behind, dirty, mergeable, files[], touches_ci}`.
- [ ] `behind` counts commits on the target the branch lacks.
- [ ] `mergeable` comes from `git merge-tree --write-tree`, which touches no working tree.
- [ ] `dirty` reflects uncommitted work in the session's worktree.
- [ ] All git work runs on the host in the project repository; no container is involved.
- [ ] A missing or pruned worktree degrades to a clear 409 rather than a 500.

---

### HS-053 — Changes view: unified diff

**As** the operator **I want** to read the actual diff on my phone **so that** review does not require a laptop.

- **Size:** S · **Blocked by:** HS-052 · **Spec:** item 12
- **Demo:** fetch the diff for one file and for the whole branch; a 2 MB file returns a truncated response with a clear marker.

**Acceptance**
- [ ] `GET /api/sessions/:id/changes/diff?path=` returns a unified diff.
- [ ] Response is capped at 256 KB with an explicit truncation marker.
- [ ] Computed with `--no-ext-diff --no-textconv`, so repository config cannot run anything.
- [ ] Binary files are summarised, not emitted.
- [ ] `path` is validated against the changed-file list; traversal is impossible.

---

### HS-054 — Merge: fast-forward path

**As** the operator **I want** to merge a finished session with one tap **so that** work actually lands.

- **Size:** M · **Blocked by:** HS-052 · **Spec:** item 12 (rules 2, 4)
- **Demo:** merge a session whose branch is ahead of the target; the target advances and the response names the new commit.

**Acceptance**
- [ ] `POST /api/sessions/:id/merge` with `{target, strategy, message?, delete_session?}`; `strategy` defaults to `ff-only`.
- [ ] `target` must be a local branch listed in `merge.targets` (default `main` and `master`, whichever exist); otherwise 422.
- [ ] `ff-only` fast-forwards directly without a temporary worktree and fails with 409 `not_fast_forward` if the target moved.
- [ ] If the target is not checked out anywhere, update it with `git update-ref refs/heads/<target> <new> <old>` (compare-and-swap).
- [ ] If the target is checked out in the main checkout, run `git merge --ff-only <new>` there; local changes that would be overwritten → 409 `target_dirty`.
- [ ] Returns `{merged_commit, target, strategy}`.

---

### HS-055 — Merge: `merge` and `squash` strategies

**As** the operator **I want** real merges and squashes **so that** a messy session history does not become my history.

- **Size:** M · **Blocked by:** HS-054 · **Spec:** item 12 (rules 2, 3)
- **Demo:** squash a three-commit session into one; force a conflict and get 409 `conflict` with the paths, with the target unchanged.

**Acceptance**
- [ ] `merge` and `squash` run in a temporary detached worktree at `<data>/merge/<id>` created from the target, removed afterwards even on failure.
- [ ] Git runs with `-c core.hooksPath=/dev/null`, so repository hooks never run agent-written code.
- [ ] Only one merge per project at a time; a concurrent attempt waits or returns 409 `merge_in_progress`.
- [ ] On conflict the merge is aborted, nothing changes, and the response is 409 `conflict` with the conflicting paths.
- [ ] The UI offers "Resolve with agent", which prompts the session to merge the target into its branch and fix conflicts, then retries.

---

### HS-056 — Merge guards and `delete_session`

**As** the operator **I want** merge to refuse while work is in flight **so that** I cannot merge half a turn.

- **Size:** S · **Blocked by:** HS-055 · **Spec:** item 12 (rules 1, 5, 6)
- **Demo:** merge during a running turn → 409 `session_running`; merge with a dirty worktree → 409 `dirty_worktree` with the commit offer.

**Acceptance**
- [ ] 409 `session_running` while a turn runs.
- [ ] 409 `dirty_worktree` while the session has uncommitted work; the UI offers to prompt the agent to commit with a clear message.
- [ ] `delete_session: true` runs the delete flow after a successful merge; the branch is then deleted because it is merged.
- [ ] Merge and publish use a 60 second timeout; other calls keep the 5 second default.
- [ ] A merge that fails after the temporary worktree is created leaves no directory behind.

---

### HS-057 — Publish

**As** the operator **I want** to push a session branch **so that** I can open a PR without a laptop.

- **Size:** M · **Blocked by:** HS-054 · **Spec:** item 12
- **Demo:** with `publish.remotes = ["origin"]`, publish pushes `agent/<id>`; with it empty, the endpoint is disabled.

**Acceptance**
- [ ] `POST /api/sessions/:id/publish` with `{remote, branch?, confirm:true}`; pushes using the host's own git credentials.
- [ ] Only remotes named in `publish.remotes` are allowed; empty config disables the endpoint (404 or 403 with a clear code).
- [ ] Never force-pushes; refuses protected names (`main`, `master`, `release/*`).
- [ ] `touches_ci` is true when the diff changes `.github/`, other CI files or hook directories, and the UI warns before confirm.
- [ ] Pushing the merged target branch is not supported and returns a clear refusal.
- [ ] Containers never hold a git credential — asserted by a test.

---

### HS-058 — UI: the Changes sheet

**As** the operator **I want** review and merge on the session screen **so that** the phone is enough.

- **Size:** M · **Blocked by:** HS-057 · **Spec:** item 12 (UI)
- **Demo:** open a session, open Changes, browse files, read a diff, pick a target and strategy, merge, and see the target advance.

**Acceptance**
- [ ] File list with status, additions and deletions; tapping a file shows its diff.
- [ ] Ahead and behind counts, plus a `touches_ci` warning where relevant.
- [ ] Target and strategy pickers, preselecting the first configured target and `ff-only`.
- [ ] Merge button disabled while running or dirty, with the reason shown and the "ask the agent to commit" shortcut offered.
- [ ] Publish button visible only when configured, with a confirm step and the CI warning.
- [ ] Conflict state shows the paths and the "Resolve with agent" action.

---

### HS-059 — Event coalescing

**As** the operator **I want** streaming to not bloat the database **so that** a long session does not make the app slow.

- **Size:** S · **Blocked by:** HS-009 · **Spec:** item 15
- **Demo:** a 5,000-delta turn produces far fewer rows than deltas, and the transcript is identical.

**Acceptance**
- [ ] Within each 25 ms commit, consecutive deltas of the same kind and message become one row with one seq.
- [ ] Coalescing never crosses message boundaries or kinds.
- [ ] The rendered transcript is byte-identical to the uncoalesced version (test compares both paths).
- [ ] Row-count reduction is measured and reported in the story.

---

### HS-060 — Message snapshots and compaction

**As** the operator **I want** old messages compressed **so that** history stays bounded without losing content.

- **Size:** M · **Blocked by:** HS-059 · **Spec:** item 15
- **Demo:** age a message past 24 hours, run the job, and the deltas are replaced by one `message.snapshot` that the client applies by message id.

**Acceptance**
- [ ] A compaction job replaces delta rows of messages completed more than 24 hours ago with one `message.snapshot` event holding full text, thinking and message id.
- [ ] The snapshot takes the highest seq of the removed rows, so seq stays strictly increasing with gaps only where compaction removed rows.
- [ ] Clients apply a snapshot by message id, replacing rather than appending, and it is safe to receive after partial deltas.
- [ ] Compaction is per session, batched, and never runs while a turn is running for that session.
- [ ] A test replays a client through a compaction boundary and asserts an identical transcript.

---

### HS-061 — History truncation and pagination

**As** the operator **I want** a new device to open a session quickly **so that** opening a 200k-event session does not hang my phone.

- **Size:** M · **Blocked by:** HS-060 · **Spec:** item 15
- **Demo:** a fresh client gets the last ~50 messages plus `history.truncated`; scrolling up loads older pages.

**Acceptance**
- [ ] With no `Last-Event-ID`, the stream starts at the beginning of the 50th most recent message and sends `history.truncated {before_seq}`.
- [ ] `GET /api/sessions/:id/events/history?before=<seq>&limit=200` returns older events in order.
- [ ] Pagination is stable under concurrent writes: a page's content does not shift because new events arrived.
- [ ] The UI loads older history on demand and shows a "load earlier" affordance, not an infinite scroll that thrashes.
- [ ] The 50-message window is configurable.

---

### HS-062 — Disk floor

**As** the operator **I want** creation refused when the disk is nearly full **so that** I do not corrupt the machine I depend on.

- **Size:** S · **Blocked by:** HS-046 · **Spec:** item 14
- **Demo:** fill the disk below `min_free_gb` and see create and resume return 507 with a clear message.

**Acceptance**
- [ ] Session creation and resume return 507 when free space is below `min_free_gb` (default 20).
- [ ] The check runs before any worktree or network is created.
- [ ] `doctor` reports the same measurement, so the two never disagree.
- [ ] Running turns are not interrupted by crossing the floor; only new starts are refused.

---

### HS-063 — Per-session disk soft cap

**As** the operator **I want** one session to be stopped when it hoards disk **so that** one bad build does not starve the others.

- **Size:** M · **Blocked by:** HS-062 · **Spec:** item 14, D2
- **Demo:** grow a worktree past `session_disk_soft_gb`; the turn aborts, an `error` event with code `disk_quota` arrives, and the session stops, resumable.

- **Acceptance**
- [ ] A watchdog runs `du` on each live session's worktree every 5 minutes.
- [ ] Over `session_disk_soft_gb` (default 10) it emits an `error` event with code `disk_quota`, aborts the turn, and stops the session.
- [ ] The stop is `stopped` (resumable after cleanup), not `error`.
- [ ] The soft cap includes `target/` and `node_modules`; the docs say to raise it for large Rust projects.
- [ ] No cache directory is shared between sessions (D2), asserted by a test that two sessions cannot see each other's caches.

---

### HS-064 — Stopped-session expiry

**As** the operator **I want** forgotten stopped sessions cleaned up **so that** I do not prune by hand.

- **Size:** M · **Blocked by:** HS-043 · **Spec:** item 15, D3, Data model
- **Demo:** backdate `stopped_at` past `stopped_ttl_days` and watch the daily job snapshot then delete the session.

**Acceptance**
- [ ] `stopped_at` is set when the status becomes `stopped` and cleared on resume.
- [ ] A daily job deletes sessions stopped for more than `stopped_ttl_days` (default 7) using the HS-043 delete flow.
- [ ] A snapshot runs first; if it fails the session is skipped and retried the next day.
- [ ] Sessions in `error` are never auto-deleted.
- [ ] The Sessions screen shows days remaining on each stopped session.

---

### HS-065 — Daily project backup

**As** the operator **I want** a nightly bundle of each project **so that** an agent damaging the shared repository is recoverable.

- **Size:** S · **Blocked by:** HS-025 · **Spec:** item 1 (`project_backup`), Known risks
- **Demo:** enable `project_backup`, run two sessions in a day, and find exactly one bundle for that day, with seven kept.

**Acceptance**
- [ ] When `project_backup` is enabled, `git bundle create <data>/backups/<project>-<date>.bundle --all` runs before the first session of each day on a project.
- [ ] The last seven bundles are kept per project; older ones are deleted.
- [ ] Failure to create a bundle is logged and does not block session creation.
- [ ] Bundles are verified with `git bundle verify` and a corrupt bundle is deleted rather than kept.

---

### HS-066 — Web Push backend

**As** the operator **I want** a notification when a long turn finishes **so that** I can put my phone down.

- **Size:** M · **Blocked by:** HS-049 · **Spec:** item 11
- **Demo:** subscribe from a phone, start a turn that runs 30 s, lock the phone, get a notification that opens that session.

**Acceptance**
- [ ] `POST /api/push/subscribe` and `DELETE /api/push/subscribe` manage rows in `push_subscriptions` (`endpoint` PK, `p256dh`, `auth`, `created_at`).
- [ ] VAPID keys from HS-049 sign every send; dead endpoints (404/410) are removed.
- [ ] Triggers: a turn that ran longer than 20 s, `session.status` error, and a budget or timeout stop.
- [ ] No notification is sent while an SSE stream for that session is open.
- [ ] Payload carries only `{host, session_id, kind}` — never content.
- [ ] The server, not the containers, holds the outbound access to the push services.

---

### HS-067 — Installable app and notification permission

**As** the operator **I want** the app on my home screen with notifications enabled **so that** iOS actually delivers them.

- **Size:** S · **Blocked by:** HS-066 · **Spec:** item 11
- **Demo:** add to home screen on iOS, grant notifications, receive a push.

**Acceptance**
- [ ] The web app is a valid installable PWA (manifest, icons, service worker) served from the control origin.
- [ ] A settings screen subscribes and unsubscribes, and shows the current state per machine.
- [ ] The iOS "must be installed to home screen" requirement is stated in the UI, not just in docs.
- [ ] Tapping a notification opens the named session.
- [ ] Each machine's keys are independent, so the app subscribes per machine it uses.
