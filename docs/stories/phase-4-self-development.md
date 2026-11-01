# Phase 4 — Self-development

**Goal:** Holosmith ships Holosmith: an agent builds the new binary, you review the diff, and a command promotes it with an automatic rollback if it fails to come up.

**Exit criteria:** `holosmith upgrade <binary>` builds nothing on the host, smoke-tests the candidate against the real Docker daemon with a replaying fake agent, drains, backs up, switches the symlink, restarts, verifies health, and rolls back on its own if the new version does not answer. `holosmith rollback` reverses a promotion that passed its health check but turned out to be wrong.

---

### HS-075 — Expand/contract migration discipline

**As** the maintainer **I want** migrations that a rollback can survive **so that** switching the symlink back does not need a database restore.

- **Size:** M · **Blocked by:** HS-004 · **Spec:** item 10
- **Demo:** a CI check rejects a migration that drops or renames a column the previous release still reads.

**Acceptance**
- [ ] A release may add but never drop or rename anything the previous release uses; cleanup ships one release later.
- [ ] A test asserts the previous release's schema is still a subset of the current one.
- [ ] `doctor` reports schema version and refuses to start when the database is newer than the binary.
- [ ] The rule is documented where a migration author will read it, with an example of the deferred cleanup.

---

### HS-076 — Build in a pinned builder container

**As** the maintainer **I want** builds to never run on my host **so that** an agent-authored `build.rs` cannot execute outside a container.

- **Size:** M · **Blocked by:** HS-048 · **Spec:** item 10 (step 1), Build and release
- **Demo:** a build runs to completion with the host having no Rust toolchain installed.

**Acceptance**
- [ ] The builder image is pinned by digest and builds the static binary for the requested target.
- [ ] No `build.rs`, proc macro or toolchain step runs host-side; the host only orchestrates Docker.
- [ ] The output binary and its SHA-256 are placed where `holosmith upgrade` expects them.
- [ ] Build failures surface the container's log tail rather than a generic error.
- [ ] The diff review (HS-058) is documented as the real gate; this story only makes the build safe, not trusted.

---

### HS-077 — Fake pi, recorded fixture and the smoke harness

**As** the maintainer **I want** a candidate binary to prove itself against real Docker before going live **so that** a bad release is caught by a robot, not by me at midnight.

- **Size:** L · **Blocked by:** HS-076 · **Spec:** item 10 (step 2), Internal subcommands
- **Demo:** the smoke harness creates a session, runs a turn and deletes it against the real Docker daemon, using a replayed session and no API key.

**Acceptance**
- [ ] `holosmith container fake-pi` replays a recorded session from inside the container, including deltas, a tool call and usage.
- [ ] A fixture recorded from a real turn (HS-032) is committed and versioned with the schema it was recorded against.
- [ ] The smoke run uses a smoke config: alternate port, temporary data directory, `container_prefix=smoke_`, `network_prefix=smoke_`, so it cannot touch real state.
- [ ] The harness creates a session, runs a turn, asserts events, and deletes it; any failure is a failed promotion.
- [ ] The harness runs against the real Docker daemon, so container, git and proxy paths are covered — the dev instance does not cover those and the docs say so.

---

### HS-078 — `holosmith upgrade <binary>`

**As** the operator **I want** one command to promote a new version safely **so that** self-development is not a manual ritual.

- **Size:** L · **Blocked by:** HS-077, HS-051, HS-075 · **Spec:** item 10, Commands table
- **Demo:** upgrade to a deliberately broken binary and watch it roll back by itself; upgrade to a good one and see the new version serving.

**Acceptance**
- [ ] Runs the spec's pipeline in order: review gate acknowledged, host smoke test, drain `--wait 10m`, DB backup with `VACUUM INTO backups/app-<ts>.db` keeping five, symlink switch, `systemctl restart`.
- [ ] Polls `/api/health` for 20 seconds; if the new version does not answer, switches the symlink back and restarts.
- [ ] Sessions live before the drain return to `idle` afterwards.
- [ ] Rebuilds the two images only when the binary or a pinned version changed.
- [ ] Refuses to upgrade while draining is already in progress, or when the candidate's schema is older than the database's.
- [ ] Prints the outcome with the version that is now live and the backup path.

---

### HS-079 — `holosmith rollback`

**As** the operator **I want** to go back one version **so that** a release that passed its health check but is wrong is one command away from gone.

- **Size:** S · **Blocked by:** HS-078 · **Spec:** item 10, Commands table
- **Demo:** roll back a live version and see the previous one serving with its sessions intact.

**Acceptance**
- [ ] Points `current` at the previous release, drains first, restarts, and verifies health.
- [ ] Refuses when the previous release's schema expectation is violated, with a clear explanation.
- [ ] Sessions survive the rollback in `idle` or `stopped`, never `error` because of the rollback itself.
- [ ] A rollback that fails its own health check says so loudly and leaves the operator a stated next step.

---

### HS-080 — Release layout and `current` symlink

**As** the maintainer **I want** releases side by side with one live pointer **so that** upgrade and rollback are atomic and reversible.

- **Size:** S · **Blocked by:** HS-047 · **Spec:** item 10, Single binary and CLI
- **Demo:** two release directories exist and `systemctl status` shows the unit executing through `current`.

**Acceptance**
- [ ] Releases live in `releases/<tag>/`; `current` is a symlink that systemd executes through.
- [ ] Switching `current` is atomic (symlink replace, never edit-in-place).
- [ ] `holosmith version` reports the release tag it was run from, so the live version is unambiguous.
- [ ] Old releases are pruned to a configured count, never the live one or the rollback target.
- [ ] `doctor` reports the resolved release path and whether the symlink is intact.
