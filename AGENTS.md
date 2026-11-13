# Holosmith

Holosmith runs coding agents in containers on machines you own, and lets you drive them from a browser or phone. One static `holosmith` binary is the server, the hub, the proxies and the deploy tool; `web/` is the app it embeds.

The README is the human overview. This file is what an agent needs before changing code.

## Linear scope

This repository tracks work in exactly one Linear project:

| Field | Value |
|---|---|
| Project | **Holosmith** |
| Project ID | `P-PER-2` |
| Project UUID | `f3a9d020-848a-437b-baec-eba402b25b15` |
| Team | `Personal` (`PER`) |
| URL | https://linear.app/adamolsson/project/holosmith-761104bbb042 |

Rules:

- Always pass `project: "Holosmith"` when calling `list_issues` or `save_issue`.
- When creating an issue, set the Holosmith project and the `PER` team explicitly. Never leave either implicit.
- Never create, read, update, comment on, or close issues in another project. In particular, `VulkanRenderAPI` belongs to a different repository and is out of scope here — do not list it, do not borrow its issue numbers, and do not treat it as precedent.
- If a Linear call cannot be scoped to Holosmith, report that to the user and stop. Do not fall back to a workspace-wide query.

Issue tracking is remote only. Do not add `HS-*`/`PER-*` citations to code, comments, help text, or docs, and do not recreate `docs/stories`, `docs/review`, `docs/notes.md`, `RESUME.md`, `PROMPT.md`, or `HolosmithSpec.md`.

## Repository map

| Path | What lives there |
|---|---|
| `crates/holosmith/src/` | The binary: CLI, HTTP, sessions, events, Docker backend, proxies, deploy/release |
| `crates/holosmith/tests/` | Integration tests; `tests/support/` and `tests/fixtures/` hold shared helpers and real pi recordings |
| `crates/holosmith/migrations/` | Embedded, forward-only SQL migrations |
| `crates/holosmith/images/` | Embedded Dockerfiles for the agent, proxy, builder and smoke images |
| `crates/holosmith/fixtures/` | The recorded turn the smoke image replays |
| `web/` | Vite + TypeScript PWA; built into `web/dist` and embedded in the binary |
| `scripts/` | Build, static-artifact checks, embed checks and the end-to-end gate |
| `docs/build.md` | How to build, test, verify, deploy and measure the credential path by hand |
| `docs/quickstart.md` | One machine, phone as the browser: config, deploy, first session |
| `docs/config.md` | Every config key: type, default, rule, and what reads it |
| `docs/adr/` | Architecture decisions, one decision per file |
| `docs/schema/previous.sql` | The previous release's schema; `tests/schema_previous.rs` compares it |
| `pins/pi.txt` | The single pinned pi version |
| `GLOSSARY.md` | The project vocabulary — read it before naming anything |

## Build and gates

The workspace is Rust 2024, `rust-version = "1.99"`, one binary crate. `unsafe_code` is denied and `unwrap_used` is warned; CI treats warnings as errors.

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
scripts/verify.sh            # the full gate: fmt, clippy, tests, migrations, build, embed, booted artifact
```

`scripts/verify.sh` is one command for all of it: the three gates above, the artifact build and its
checks, and the boot of the shipped binary. Follow `docs/build.md` for the environment and the artifact
build; do not invent a second build path.

`scripts/verify.sh` needs the musl cross toolchains on `PATH` **before** it runs, and the default
`~/.cargo`/`~/.rustup` toolchain does not have them — its `build` phase fails with a missing
`x86_64-unknown-linux-musl`, which reads like a code error and is not. Source the machine-local env first:

```bash
. ~/.pi/toolchains/env.sh     # rustc 1.99 + both musl targets + the cross gccs + node/npm
scripts/verify.sh
```

That is a full run of all seven phases, about fifteen minutes on this host, and it ends with one line to
check for: `verify: <n> assertions, ok` — 2207 at the time of writing. The count moves as cases are
added; `ok` on that last line is what green looks like, and any `FAIL case_x/assertion` line above it
names the case and the assertion. It also wants a working Docker daemon for the container cases; a case
that cannot measure its fact prints `SKIP` with the reason instead of passing quietly.

Two things this gate teaches the hard way. `doctor`'s `perms` check reads the file `--config` names and
compares it against the 0644 `server deploy` writes, so a fixture that writes a config with a redirect
under the shell's umask (0664) fails a case that has nothing to do with modes — write fixture configs
with `chmod 0644`, as `deploy_config` does. And the `serve` phase boots `dist/<host-triple>/holosmith`,
the artifact the `build` phase just produced, never `target/debug`.

The web app has its own gate:

```bash
cd web && npm ci && npm test && npm run build
```

## Invariants an agent must not break

- **One binary, three roles.** `serve` runs the role in the config: `hub`, `server`, or `both`. A hub has no Docker and no sessions; it forwards. A server runs containers and owns the session API.
- **The event log is the source of truth for a session's transcript.** One writer commits batches to SQLite (WAL, `synchronous=FULL`), then broadcasts. Do not add a second writer or bypass the transition table in `sessions/state.rs`.
- **All git work happens on the host.** Sessions get a git worktree and an `agent/<id>` branch; the server merges, diffs, snapshots and publishes. A container never owns the repository.
- **Isolation is topology, not configuration.** Per-session internal Docker networks, an egress proxy for allow-listed HTTPS, a key proxy that keeps provider keys out of containers, hardened read-only `.git` mounts, `--cap-drop ALL`, no Docker socket in a session.
- **Two listeners.** The control listener serves the API and the app; the preview listener serves only `/host/id/*` on its own origin. Do not merge them.
- **Forward-only migrations.** Add columns and tables; never drop or rename something the previous release reads. `docs/schema/previous.sql` is the snapshot the contract test compares.
- **The web app is embedded.** `build.rs` embeds `web/dist` (or a placeholder when it is absent). A release build must set `HOLOSMITH_REQUIRE_WEB=1`; `scripts/build.sh` does.
- **No browser in the gate.** Browser-only behaviour belongs in `web/test/` (`npm test`), not in `scripts/verify.sh`.

## Working style

- Read the neighbouring module's doc comment before changing it; the comments carry the reasons, not just the types.
- Prefer one spelling for one idea. The glossary records the chosen word and the words to avoid.
- Add a test at the layer that can actually fail: a unit test for a rule, an integration test for a wire shape, a `scripts/verify.sh` case for a shipped artifact or a real daemon.
- When a change alters a decision, add or update an ADR in `docs/adr/`. Decisions are the durable record now that the old story/spec files are gone.
- Keep the CLI help and user-facing sentences free of internal references.
