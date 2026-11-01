# Phase 0 — Walking skeleton

**Goal:** a single static binary that runs, loads config, opens a database, serves an embedded web page and a health endpoint, and refuses malformed requests.

**Exit criteria:** `holosmith serve` on a dev machine serves the app shell at the control origin; `GET /api/health` returns name, version and role; the database exists with a schema version; cross-origin and non-conforming requests are rejected; `scripts/verify.sh` proves all of it in one command.

No sessions, no Docker, no git. This phase exists so that everything after it lands on a running, testable, guarded binary.

---

### HS-001 — Static binary and `holosmith version`

**As** the operator **I want** a single static binary that reports what it is **so that** I can put the same artifact on any Linux machine and know exactly what is running.

- **Size:** S · **Blocked by:** — · **Spec:** Single binary, Build and release, CLI phase 1
- **Demo:** `./holosmith version` runs on x86_64 and aarch64 with no dynamic dependencies.

**Acceptance**
- [ ] Build produces `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` artifacts; `file` reports statically linked; both run in an empty container.
- [ ] `holosmith version` prints version, git tag, pinned pi version and schema version, one per line, machine-readable with `--json`.
- [ ] The pinned pi version exists in exactly one place that the build, `doctor` and the agent image all read.
- [ ] The build fetches nothing at build time except declared dependencies.

**Notes**
- Rust is assumed: the spec's "build.rs and proc macros never run host-side" only makes sense for Rust, and musl static + bundled SQLite is a Rust-shaped requirement.

---

### HS-002 — Config: load, validate, show, edit

**As** the operator **I want** one config file that rejects typos **so that** a mistyped key cannot silently fall back to a default I did not intend.

- **Size:** S · **Blocked by:** HS-001 · **Spec:** Config, CLI phase 1
- **Demo:** `holosmith config validate` fails loudly on `turn_timeout_mni`; `config show` prints every resolved value.

**Acceptance**
- [ ] One TOML file: role, control and preview listen addresses, data root, projects root, `docker_host`, hosts seed, `allowed_hubs`, egress allow-list.
- [ ] Every amendment key is present with its spec default: `network_default` (off), `merge.targets` (`main`, `master`), `publish.remotes` (empty), `project_backup` (off), `turn_timeout_min` (45), `idle_stop_min` (30), `max_session_tokens` (unset), `min_free_gb` (20), `session_disk_soft_gb` (10), `stopped_ttl_days` (7), `abort_grace` (10s).
- [ ] `config validate` rejects unknown keys, reports every problem at once, exits non-zero.
- [ ] `config show` prints resolved values including defaults, redacts nothing but has nothing secret to show (API keys live in the secrets file, not here).
- [ ] `serve` with no config file starts on defaults and logs the path it would have used.
- [ ] `config edit` opens `$EDITOR` and re-validates on save.

**Notes**
- The session cap has no spec key. Add `max_live_sessions` (default 3) here so HS-020 has something to read.

---

### HS-003 — `serve`, control listener, embedded web app, `/api/health`

**As** the operator **I want** to open the app in a browser **so that** there is a running thing to build on.

- **Size:** S · **Blocked by:** HS-002 · **Spec:** Roles, CLI phase 1, item 4
- **Demo:** `holosmith serve` then `curl /api/health` and a browser showing the app shell.

**Acceptance**
- [ ] `holosmith serve [--config PATH]` runs in the foreground with the role from config, logging the bound addresses.
- [ ] The web app is built first and embedded in the binary; serving works with no files on disk next to the binary.
- [ ] `GET /api/health` returns `{name: "holosmith", version, role, uptime_s}` and is the only unauthenticated-by-role endpoint.
- [ ] SIGTERM and SIGINT shut down gracefully: stop accepting, finish in-flight requests, exit 0.
- [ ] `--backend` and `--port`/`--data-dir` overrides exist for the dev instance and smoke tests.

**Notes**
- Frontend: Vite + TypeScript, single page, no SSR, no meta-framework. Reversible; noted so later stories do not re-litigate it.

---

### HS-004 — SQLite with embedded forward-only migrations

**As** the operator **I want** the database schema to be created and upgraded by the binary itself **so that** there is no separate migration step to forget.

- **Size:** S · **Blocked by:** HS-003 · **Spec:** item 10 (migrations), item 5 (WAL, synchronous=FULL)
- **Demo:** first start creates `app.db` and reports schema 1; second start is a no-op; a deliberately broken migration refuses to start and leaves the DB untouched.

**Acceptance**
- [ ] SQLite is bundled (no system library); DB at `<data>/app.db`.
- [ ] On open: `journal_mode=WAL`, `synchronous=FULL`, foreign keys on, busy timeout set.
- [ ] Migrations are numbered SQL files embedded in the binary, applied in a single transaction at startup, recorded in a `schema_version` table.
- [ ] A failing migration rolls back and exits non-zero with the file name and statement.
- [ ] `holosmith version --json` reports the schema version.
- [ ] Two processes cannot migrate concurrently: a startup lock makes the second wait or fail cleanly.

---

### HS-005 — Browser-facing request guards

**As** the operator **I want** the control listener to reject anything that is not my browser **so that** a malicious page cannot drive my machine.

- **Size:** M · **Blocked by:** HS-003 · **Spec:** item 4
- **Demo:** a cross-origin form POST is rejected, a POST without `X-Holosmith-Client` is rejected, and the real client still works.

**Acceptance**
- [ ] `Host` is checked against an allow-list (configured names, `localhost`, `127.0.0.1`); mismatch → 403 `bad_host`.
- [ ] Every non-GET request checks `Origin` against the control origin; mismatch or missing → 403 `bad_origin`.
- [ ] Every non-GET request requires `Content-Type: application/json` and `X-Holosmith-Client: 1`; missing → 403 `bad_client`.
- [ ] The server sends no CORS headers, ever, including on preflight.
- [ ] When `owner` is configured, requests carrying `Tailscale-User-Login` must match it; a mismatch → 403 `bad_owner`.
- [ ] GET handlers cannot mutate state: a test asserts that no mutating handler is registered on a GET route.
- [ ] Guards are middleware that a route cannot opt out of, so later stories inherit them by existing.

---

### HS-006 — One-command verification harness

**As** the maintainer **I want** a single command that proves the build works **so that** every later story can extend it instead of inventing its own check.

- **Size:** S · **Blocked by:** HS-005 · **Spec:** item 10 (smoke test precedent), V1–V11
- **Demo:** `scripts/verify.sh` goes green from a clean checkout and red when any guard is removed.

**Acceptance**
- [ ] `scripts/verify.sh` builds, runs unit tests, then boots the real binary on a random port with a temp data dir.
- [ ] It asserts `/api/health`, the guard rejections from HS-005, and the schema version, then tears the process down.
- [ ] Failure output names the failing assertion and leaves the temp data dir for inspection.
- [ ] A convention is documented: every later story adds at least one case here.
